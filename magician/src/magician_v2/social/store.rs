use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Notify;

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ScopeRef};

use super::types::{
    contains_secret_shaped_content, Group, GroupMembershipRow, Member, MentionRow,
    OperatorPolicyRow, PendingMention, Post, Reaction, SelfState,
};

pub const MAX_MENTIONS_PER_POST: usize = 8;
const MAX_SOCIAL_POST_CHARS: usize = 10_000;
/// Public because the HTTP surface enforces it now: the store no longer sees
/// a group write, so the rule has to live where the write does.
pub const MAX_SOCIAL_GROUP_MEMBERS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SocialScopeKey {
    pub principal: String,
    pub workspace: String,
}

impl SocialScopeKey {
    pub fn new(principal: &str, workspace: &str) -> Self {
        let (principal, workspace) = ArtifactV2Workspace::scope_dir_segments(principal, workspace);
        Self {
            principal,
            workspace,
        }
    }
}

/// A bounded, coalescing wake queue. One hot workspace occupies one set entry
/// rather than one channel item per post, while the scope identity prevents a
/// mention from rescanning every configured workspace.
#[derive(Default)]
pub struct SocialWakeQueue {
    notify: Notify,
    dirty_scopes: Mutex<HashSet<SocialScopeKey>>,
    full_tick: AtomicBool,
}

impl SocialWakeQueue {
    pub fn mark_scope(&self, scope: SocialScopeKey) {
        // A panic in an unrelated owner must not permanently disable durable
        // mention delivery. Recover the set from a poisoned mutex and keep the
        // wake net live; the SQLite inbox remains the source of truth.
        let inserted = match self.dirty_scopes.lock() {
            Ok(mut scopes) => scopes.insert(scope),
            Err(poisoned) => poisoned.into_inner().insert(scope),
        };
        if inserted {
            self.notify.notify_one();
        }
    }

    /// Wake the worker for a full cadence-equivalent tick (ambient invites
    /// plus mention backlog), not a mention-only drain.
    pub fn request_full_tick(&self) {
        self.full_tick.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    pub fn take_full_tick(&self) -> bool {
        self.full_tick.swap(false, Ordering::SeqCst)
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    pub fn take_dirty_scopes(&self) -> HashSet<SocialScopeKey> {
        match self.dirty_scopes.lock() {
            Ok(mut scopes) => std::mem::take(&mut *scopes),
            Err(poisoned) => {
                let mut scopes = poisoned.into_inner();
                std::mem::take(&mut *scopes)
            },
        }
    }
}

const BOOTSTRAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS members (
  member_id     TEXT PRIMARY KEY,
  kind          TEXT NOT NULL,
  display_name  TEXT NOT NULL,
  introversion  REAL NOT NULL,
  opted_out     INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS self_state (
  member_id     TEXT PRIMARY KEY REFERENCES members(member_id),
  valence       REAL NOT NULL,
  energy        REAL NOT NULL,
  baseline_valence REAL NOT NULL,
  baseline_energy  REAL NOT NULL,
  note          TEXT,
  updated_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS posts (
  post_id       TEXT PRIMARY KEY,
  author_id     TEXT NOT NULL REFERENCES members(member_id),
  surface       TEXT NOT NULL,
  group_id      TEXT REFERENCES groups(group_id),
  post_type     TEXT NOT NULL,
  body          TEXT NOT NULL,
  parent_id     TEXT REFERENCES posts(post_id),
  created_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS posts_surface_created_id ON posts(surface, created_at DESC, post_id DESC);
DROP INDEX IF EXISTS posts_surface_created;
CREATE INDEX IF NOT EXISTS posts_parent ON posts(parent_id);
CREATE INDEX IF NOT EXISTS posts_author_created_id ON posts(author_id, created_at DESC, post_id DESC);
CREATE INDEX IF NOT EXISTS posts_created_id ON posts(created_at, post_id);
CREATE INDEX IF NOT EXISTS posts_group_created_id ON posts(group_id, created_at DESC, post_id DESC);

CREATE TABLE IF NOT EXISTS mentions (
  post_id              TEXT NOT NULL REFERENCES posts(post_id),
  mentioned_member_id  TEXT NOT NULL REFERENCES members(member_id),
  delivery_kind        TEXT NOT NULL DEFAULT 'explicit_mention',
  status               TEXT NOT NULL DEFAULT 'pending',
  created_at           TEXT NOT NULL DEFAULT '',
  handled_at           TEXT,
  response_post_id     TEXT REFERENCES posts(post_id),
  PRIMARY KEY (post_id, mentioned_member_id)
);
CREATE TABLE IF NOT EXISTS reactions (
  post_id       TEXT NOT NULL REFERENCES posts(post_id),
  member_id     TEXT NOT NULL REFERENCES members(member_id),
  emoji         TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  PRIMARY KEY (post_id, member_id, emoji)
);

CREATE TABLE IF NOT EXISTS groups (
  group_id      TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  created_by    TEXT NOT NULL REFERENCES members(member_id),
  created_at    TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS group_members (
  group_id      TEXT NOT NULL REFERENCES groups(group_id),
  member_id     TEXT NOT NULL REFERENCES members(member_id),
  PRIMARY KEY (group_id, member_id)
);
CREATE INDEX IF NOT EXISTS group_members_member_group
  ON group_members(member_id, group_id);

CREATE TABLE IF NOT EXISTS operator_policy (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  autonomous_enabled INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL
);
"#;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SocialRetentionReport {
    pub posts_removed: usize,
    pub spend_rows_removed: usize,
}

/// Lazily opens one independent SQLite social store per authenticated scope.
/// The registry is process-shared, while every database remains physically
/// rooted below `scopes/<principal>/<workspace>/social/`.
#[derive(Clone)]
pub struct SocialStoreRegistry {
    workspace: ArtifactV2Workspace,
    stores: Arc<Mutex<HashMap<(String, String), Arc<SocialStore>>>>,
    open_gates: Arc<Mutex<HashMap<(String, String), Arc<Mutex<()>>>>>,
    operator_initialized: Arc<Mutex<HashSet<(String, String)>>>,
    mention_wake: Arc<SocialWakeQueue>,
}

impl SocialStoreRegistry {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self {
            workspace,
            stores: Arc::new(Mutex::new(HashMap::new())),
            open_gates: Arc::new(Mutex::new(HashMap::new())),
            operator_initialized: Arc::new(Mutex::new(HashSet::new())),
            mention_wake: Arc::new(SocialWakeQueue::default()),
        }
    }

    pub fn store_for_scope(&self, scope: &ScopeRef) -> Result<Arc<SocialStore>> {
        let (principal, workspace) =
            ArtifactV2Workspace::scope_dir_segments(&scope.principal(), &scope.workspace());
        let key = (principal.clone(), workspace.clone());
        let cached = self
            .stores
            .lock()
            .map_err(|_| anyhow::anyhow!("social store registry lock poisoned"))?
            .get(&key)
            .cloned();
        if let Some(store) = cached {
            self.ensure_operator(&key, &principal, &store)?;
            return Ok(store);
        }

        // Opening bootstraps/migrates SQLite, so the ordinary check-then-open
        // cache pattern is not safe: the initial UI projection starts several
        // scoped reads concurrently. Serialize only the same scope, recheck
        // after acquiring its gate, and let unrelated workspaces open in
        // parallel.
        let open_gate = self.open_gate_for(&key)?;
        let _open_guard = open_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(store) = self
            .stores
            .lock()
            .map_err(|_| anyhow::anyhow!("social store registry lock poisoned"))?
            .get(&key)
            .cloned()
        {
            self.ensure_operator(&key, &principal, &store)?;
            return Ok(store);
        }

        let root = self.checked_social_root(&principal, &workspace)?;
        self.migrate_legacy_default_store_if_needed(&principal, &workspace, &root)?;
        let store = Arc::new(SocialStore::open_with_mention_wake(
            &root,
            Arc::clone(&self.mention_wake),
            SocialScopeKey::new(&principal, &workspace),
        )?);
        self.stores
            .lock()
            .map_err(|_| anyhow::anyhow!("social store registry lock poisoned"))?
            .insert(key.clone(), Arc::clone(&store));
        self.ensure_operator(&key, &principal, &store)?;
        Ok(store)
    }

    fn open_gate_for(&self, key: &(String, String)) -> Result<Arc<Mutex<()>>> {
        let mut gates = self
            .open_gates
            .lock()
            .map_err(|_| anyhow::anyhow!("social open-gate registry lock poisoned"))?;
        Ok(Arc::clone(
            gates
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        ))
    }

    fn cached_store(&self, key: &(String, String)) -> Result<Option<Arc<SocialStore>>> {
        Ok(self
            .stores
            .lock()
            .map_err(|_| anyhow::anyhow!("social store registry lock poisoned"))?
            .get(key)
            .cloned())
    }

    /// Read-only consumers must not create a database merely by projecting a
    /// scope. The API and worker use `store_for_scope`; Fleet State and storage
    /// inventory use this non-materializing variant.
    pub fn existing_store_for_scope(&self, scope: &ScopeRef) -> Result<Option<Arc<SocialStore>>> {
        let (principal, workspace) =
            ArtifactV2Workspace::scope_dir_segments(&scope.principal(), &scope.workspace());
        let key = (principal.clone(), workspace.clone());
        if let Some(store) = self.cached_store(&key)? {
            return Ok(Some(store));
        }

        let open_gate = self.open_gate_for(&key)?;
        let _open_guard = open_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(store) = self.cached_store(&key)? {
            return Ok(Some(store));
        }
        let root = self.checked_social_root(&principal, &workspace)?;
        if !root.join("social.db").is_file() {
            return Ok(None);
        }
        let opened = Arc::new(SocialStore::open_with_mention_wake(
            &root,
            Arc::clone(&self.mention_wake),
            SocialScopeKey::new(&principal, &workspace),
        )?);
        let store = {
            let mut stores = self
                .stores
                .lock()
                .map_err(|_| anyhow::anyhow!("social store registry lock poisoned"))?;
            stores.entry(key).or_insert_with(|| opened.clone()).clone()
        };
        Ok(Some(store))
    }

    pub fn database_path_for_scope(&self, scope: &ScopeRef) -> PathBuf {
        crate::magician_v2::database_owners::database_file_path(
            &self.workspace,
            &scope.principal(),
            &scope.workspace(),
            crate::magician_v2::database_owners::DatabaseOwner::SocialSqlite,
        )
    }

    pub fn mention_notifier(&self) -> Arc<SocialWakeQueue> {
        Arc::clone(&self.mention_wake)
    }

    /// Reject every existing redirect below the trusted runtime root before
    /// opening SQLite. Checking only `social/` or `social.db` is insufficient:
    /// a planted `scopes/<principal>` symlink can otherwise redirect both the
    /// directory creation and database open outside the scoped store.
    fn checked_social_root(&self, principal: &str, workspace: &str) -> Result<PathBuf> {
        let mut current = self.workspace.base_root().to_path_buf();
        for segment in ["scopes", principal, workspace, "social"] {
            current.push(segment);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    anyhow::bail!(
                        "scoped social path component must not be a symbolic link: {}",
                        current.display()
                    );
                },
                Ok(_) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "inspecting scoped social path component {}",
                            current.display()
                        )
                    });
                },
            }
        }
        Ok(current)
    }

    /// The pre-scope implementation stored one process-global database at the
    /// runtime root. Its writes had no principal/workspace provenance, so the
    /// only honest upgrade target is the historical default scope. Checkpoint
    /// WAL first, then create the scoped pathname with a no-clobber hard link;
    /// a concurrent opener can never overwrite an already-created scoped DB.
    fn migrate_legacy_default_store_if_needed(
        &self,
        principal: &str,
        workspace: &str,
        scoped_root: &Path,
    ) -> Result<()> {
        if principal != "anonymous" || workspace != "default" {
            return Ok(());
        }
        let legacy = self.workspace.base_root().join("social.db");
        let target = scoped_root.join("social.db");
        if target.is_file() || !legacy.is_file() {
            return Ok(());
        }
        if std::fs::symlink_metadata(scoped_root)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            anyhow::bail!("scoped social directory must not be a symbolic link");
        }
        if std::fs::symlink_metadata(&legacy)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            anyhow::bail!("legacy social database must not be a symbolic link");
        }
        std::fs::create_dir_all(scoped_root).with_context(|| {
            format!(
                "creating scoped social directory for legacy migration: {}",
                scoped_root.display()
            )
        })?;
        {
            let conn = match Connection::open_with_flags(&legacy, OpenFlags::SQLITE_OPEN_READ_WRITE)
            {
                Ok(conn) => conn,
                Err(_) if target.is_file() => return Ok(()),
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("opening legacy social store at {}", legacy.display())
                    });
                },
            };
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
                .context("checkpointing legacy social store before scoped migration")?;
        }
        match std::fs::hard_link(&legacy, &target) {
            Ok(()) => {
                if let Err(error) = std::fs::remove_file(&legacy) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(path = %legacy.display(), %error, "scoped social migration succeeded but the legacy pathname remains");
                    }
                }
                for suffix in ["-wal", "-shm"] {
                    let sidecar = PathBuf::from(format!("{}{}", legacy.display(), suffix));
                    if let Err(error) = std::fs::remove_file(&sidecar) {
                        if error.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!(path = %sidecar.display(), %error, "could not remove a checkpointed legacy social sidecar");
                        }
                    }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists || target.is_file() => {
            },
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "linking legacy social store {} into scoped target {}",
                        legacy.display(),
                        target.display()
                    )
                });
            },
        }
        Ok(())
    }

    /// Materializing consumers share one operator-roster bootstrap per scope.
    /// This keeps concurrent feed projections from turning every read request
    /// into a serialized SQLite upsert. Non-materializing lookups never call it.
    fn ensure_operator(
        &self,
        key: &(String, String),
        display_name: &str,
        store: &SocialStore,
    ) -> Result<()> {
        if self
            .operator_initialized
            .lock()
            .map_err(|_| anyhow::anyhow!("social operator registry lock poisoned"))?
            .contains(key)
        {
            return Ok(());
        }
        // Never hold the process-wide initialization registry across SQLite.
        // Concurrent first readers may repeat this idempotent upsert, but one
        // slow scope cannot head-of-line block an unrelated scope.
        store.upsert_member(&Member {
            member_id: "operator".to_string(),
            kind: "operator".to_string(),
            display_name: display_name.to_string(),
            introversion: 0.5,
            opted_out: false,
            created_at: chrono::Utc::now().to_rfc3339(),
        })?;
        self.operator_initialized
            .lock()
            .map_err(|_| anyhow::anyhow!("social operator registry lock poisoned"))?
            .insert(key.clone());
        Ok(())
    }
}

#[derive(Clone)]
pub struct SocialStore {
    conn: Arc<Mutex<Connection>>,
    mention_wake: Option<Arc<SocialWakeQueue>>,
    scope_key: Option<SocialScopeKey>,
}

impl SocialStore {
    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("social store connection lock poisoned"))
    }

    pub fn open(base_root: &Path) -> Result<Self> {
        Self::open_inner(base_root, None)
    }

    fn open_with_mention_wake(
        base_root: &Path,
        mention_wake: Arc<SocialWakeQueue>,
        scope_key: SocialScopeKey,
    ) -> Result<Self> {
        Self::open_inner(base_root, Some((mention_wake, scope_key)))
    }

    fn open_inner(
        base_root: &Path,
        wake: Option<(Arc<SocialWakeQueue>, SocialScopeKey)>,
    ) -> Result<Self> {
        if std::fs::symlink_metadata(base_root)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            anyhow::bail!("social store directory must not be a symbolic link");
        }
        std::fs::create_dir_all(base_root)
            .with_context(|| format!("creating social store directory: {}", base_root.display()))?;
        let db_path = base_root.join("social.db");
        if std::fs::symlink_metadata(&db_path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            anyhow::bail!("social database must not be a symbolic link");
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening social store at {}", db_path.display()))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .context("enabling social-store foreign keys")?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
            .context("enabling WAL journal mode on social store")?;
        conn.execute_batch(BOOTSTRAP_DDL)
            .context("bootstrapping social store schema")?;
        migrate_mention_schema(&conn)?;
        let (mention_wake, scope_key) = wake
            .map(|(wake, scope)| (Some(wake), Some(scope)))
            .unwrap_or((None, None));
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            mention_wake,
            scope_key,
        })
    }

    pub fn open_in_temp() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory social store")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .context("enabling in-memory social-store foreign keys")?;
        conn.execute_batch(BOOTSTRAP_DDL)
            .context("bootstrapping social store schema")?;
        migrate_mention_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            mention_wake: None,
            scope_key: None,
        })
    }

    pub fn compact(
        &self,
    ) -> Result<crate::magician_v2::storage_governance::DuckDbCompactionReport> {
        let conn = self.connection()?;
        let page_count: i64 = conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap_or(0);
        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap_or(0);
        let bytes_before = (page_count * page_size) as u64;

        conn.execute("VACUUM", [])
            .context("vacuuming social store")?;

        let page_count_after: i64 = conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap_or(0);
        let bytes_after = (page_count_after * page_size) as u64;
        let bytes_reclaimed = bytes_before.saturating_sub(bytes_after);

        let row_count: i64 = conn
            .query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
            .unwrap_or(0);

        Ok(
            crate::magician_v2::storage_governance::DuckDbCompactionReport {
                database: "social.db".to_string(),
                bytes_before,
                bytes_after,
                bytes_reclaimed,
                // members, self_state, posts, mentions, reactions, groups,
                // group_members, operator_policy. The two budget tables retired
                // with the engine; an existing store may still hold them, but
                // this store no longer creates or counts them.
                table_count: 8,
                row_count: row_count as u64,
            },
        )
    }

    /// O(1) health metadata. Exact row counts belong to explicit storage
    /// diagnostics; a 30-second serving poll must not scan every social table.
    pub fn database_size_bytes(&self) -> Result<u64> {
        let conn = self.connection()?;
        let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        Ok(page_count.max(0) as u64 * page_size.max(0) as u64)
    }

    pub fn member_count(&self) -> Result<u64> {
        let conn = self.connection()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM members", [], |row| row.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// Operator opt-in for autonomous chatter. Missing row is off.
    pub fn autonomous_enabled(&self) -> Result<bool> {
        Ok(self.operator_policy()?.0)
    }

    pub fn operator_policy(&self) -> Result<(bool, Option<String>)> {
        let conn = self.connection()?;
        let row = conn
            .query_row(
                "SELECT autonomous_enabled, updated_at FROM operator_policy WHERE id = 1",
                [],
                |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((enabled, updated_at)) => (enabled, Some(updated_at)),
            None => (false, None),
        })
    }

    pub fn set_autonomous_enabled(&self, enabled: bool, updated_at: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO operator_policy (id, autonomous_enabled, updated_at)
             VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET
               autonomous_enabled = excluded.autonomous_enabled,
               updated_at = excluded.updated_at",
            params![if enabled { 1 } else { 0 }, updated_at],
        )?;
        Ok(())
    }

    /// Exact logical row count for an explicit storage-governance snapshot.
    /// Serving health deliberately uses O(1) page metadata instead; this scan
    /// is reserved for the operator-requested diagnostic path.
    pub fn diagnostic_total_row_count(&self) -> Result<u64> {
        let conn = self.connection()?;
        let count: i64 = conn.query_row(
            "SELECT
                (SELECT COUNT(*) FROM members) +
                (SELECT COUNT(*) FROM self_state) +
                (SELECT COUNT(*) FROM posts) +
                (SELECT COUNT(*) FROM mentions) +
                (SELECT COUNT(*) FROM reactions) +
                (SELECT COUNT(*) FROM groups) +
                (SELECT COUNT(*) FROM group_members) +
                (SELECT COUNT(*) FROM operator_policy)",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Incremental online retention. The batch caps keep a periodic social
    /// tick from turning cleanup into an unbounded SQLite transaction.
    ///
    /// `max_spend_rows` is retained in the signature and ignored: the spend log
    /// retired with the engine, and changing the shape of a storage-governance
    /// entry point to delete a parameter nobody passes meaningfully would be a
    /// wider change than the retirement earns.
    pub fn prune_history(
        &self,
        cutoff: &str,
        max_posts: usize,
        _max_spend_rows: usize,
    ) -> Result<SocialRetentionReport> {
        const POST_BATCH: usize = 500;
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;

        let mut victims = {
            let mut statement = tx.prepare(
                "SELECT post_id FROM posts
                 WHERE created_at < ?1
                 ORDER BY created_at ASC, post_id ASC
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(params![cutoff, POST_BATCH as i64], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut seen = victims.iter().cloned().collect::<HashSet<_>>();
        if victims.len() < POST_BATCH {
            let total: i64 = tx.query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))?;
            let excess = (total.max(0) as usize).saturating_sub(max_posts);
            if excess > 0 {
                let remaining = (POST_BATCH - victims.len()).min(excess);
                let mut statement = tx.prepare(
                    "SELECT post_id FROM posts
                     ORDER BY created_at ASC, post_id ASC
                     LIMIT ?1",
                )?;
                let rows = statement
                    .query_map(params![remaining as i64], |row| row.get::<_, String>(0))?;
                for row in rows {
                    let post_id = row?;
                    if seen.insert(post_id.clone()) {
                        victims.push(post_id);
                    }
                }
            }
        }

        for post_id in &victims {
            tx.execute("DELETE FROM reactions WHERE post_id = ?1", params![post_id])?;
            tx.execute(
                "UPDATE mentions SET response_post_id = NULL WHERE response_post_id = ?1",
                params![post_id],
            )?;
            tx.execute("DELETE FROM mentions WHERE post_id = ?1", params![post_id])?;
            tx.execute(
                "UPDATE posts SET parent_id = NULL WHERE parent_id = ?1",
                params![post_id],
            )?;
            tx.execute("DELETE FROM posts WHERE post_id = ?1", params![post_id])?;
        }

        // The spend log retired with the engine. This store no longer creates
        // the table, so it must not query it either: on a store created after
        // the retirement there is nothing to prune, and a `SELECT` would fail
        // rather than find zero rows. An older store keeps whatever it holds.
        tx.commit()?;
        Ok(SocialRetentionReport {
            posts_removed: victims.len(),
            spend_rows_removed: 0,
        })
    }

    pub fn pending_reply_notification_counts(&self) -> Result<HashMap<String, usize>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT mentioned_member_id, COUNT(*)
             FROM mentions
             WHERE delivery_kind = 'reply_notification' AND status = 'pending'
             GROUP BY mentioned_member_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as usize,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
    }

    // Members
    pub fn upsert_member(&self, member: &Member) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO members (member_id, kind, display_name, introversion, opted_out, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(member_id) DO UPDATE SET
               display_name = excluded.display_name,
               introversion = excluded.introversion,
               opted_out = excluded.opted_out
             WHERE members.display_name IS NOT excluded.display_name
                OR members.introversion IS NOT excluded.introversion
                OR members.opted_out IS NOT excluded.opted_out",
            params![
                member.member_id,
                member.kind,
                member.display_name,
                member.introversion,
                if member.opted_out { 1 } else { 0 },
                member.created_at,
            ],
        ).context("upserting member")?;
        Ok(())
    }

    pub fn get_member(&self, member_id: &str) -> Result<Option<Member>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT member_id, kind, display_name, introversion, opted_out, created_at FROM members WHERE member_id = ?1")?;
        let member = stmt
            .query_row(params![member_id], |row| {
                Ok(Member {
                    member_id: row.get(0)?,
                    kind: row.get(1)?,
                    display_name: row.get(2)?,
                    introversion: row.get(3)?,
                    opted_out: row.get::<_, i64>(4)? != 0,
                    created_at: row.get(5)?,
                })
            })
            .optional()?;
        Ok(member)
    }

    // ------------------------------------------------------------------
    // Corpus export for the Town Square migration (queue item 6, slice 3)
    //
    // One method, one connection, one transaction. Reading the eight tables
    // through eight separate lock acquisitions would give the migration a TORN
    // read: the engine is still live during slice 3, so a post inserted between
    // the posts read and the mentions read yields a mention whose post is
    // absent, and the migration's headline claim — every source row present and
    // byte-identical — would be a claim about a corpus that never existed at any
    // instant.
    //
    // Reads only. Every statement is ordered by primary key so two exports over
    // an unchanged corpus produce byte-identical output, which is what lets the
    // migration digest a table and compare it to what landed in the app store.
    // ------------------------------------------------------------------

    /// Rows this store will hand to one migration read, per table.
    ///
    /// Deliberately NOT the package manifest's `storage.max_records` (200,000),
    /// which was the first choice and the wrong one. The migration has to read
    /// every migrated row back out of the app entity store to prove it landed,
    /// and an unindexed snapshot read there refuses above
    /// `apps::entity_store::MAX_QUERY_SNAPSHOT_ROWS`. A cap above that buys a
    /// corpus that can be written and then never verified — the worst outcome
    /// for a one-shot irreversible move. Refusing early is the honest failure.
    pub const MIGRATION_MAX_ROWS_PER_TABLE: usize =
        crate::magician_v2::apps::entity_store::MAX_QUERY_SNAPSHOT_ROWS;

    /// The whole corpus, read at one instant.
    pub fn export_corpus(&self) -> Result<SocialCorpusExport> {
        let mut conn = self.connection()?;
        // Deferred is enough: this takes a read lock on first statement and
        // holds a consistent view for the rest, without blocking the live
        // engine's writers before it needs to.
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let export = SocialCorpusExport {
            members: export_members(&transaction)?,
            self_states: export_self_states(&transaction)?,
            groups: export_groups(&transaction)?,
            memberships: export_group_memberships(&transaction)?,
            posts: export_posts(&transaction)?,
            mentions: export_mentions(&transaction)?,
            reactions: export_reactions(&transaction)?,
            operator_policy: export_operator_policy(&transaction)?,
        };
        transaction.commit()?;
        export.enforce_row_bounds()?;
        Ok(export)
    }

    pub fn get_all_members(&self) -> Result<Vec<Member>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT member_id, kind, display_name, introversion, opted_out, created_at
             FROM members
             ORDER BY kind, display_name, member_id",
        )?;
        let iter = stmt.query_map([], |row| {
            Ok(Member {
                member_id: row.get(0)?,
                kind: row.get(1)?,
                display_name: row.get(2)?,
                introversion: row.get(3)?,
                opted_out: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
            })
        })?;
        let mut members = Vec::new();
        for r in iter {
            members.push(r?);
        }
        Ok(members)
    }

    /// Return the live roster used for presence and mention selection.
    ///
    /// Opted-out agents remain in `members` so historical posts can still be
    /// attributed, but they must not remain taggable after definition removal
    /// or an explicit social opt-out. The operator is always retained because
    /// it is the scope owner rather than an autonomous participant.
    pub fn get_active_members(&self) -> Result<Vec<Member>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT member_id, kind, display_name, introversion, opted_out, created_at
             FROM members
             WHERE kind = 'operator' OR opted_out = 0
             ORDER BY kind, display_name, member_id",
        )?;
        let iter = stmt.query_map([], |row| {
            Ok(Member {
                member_id: row.get(0)?,
                kind: row.get(1)?,
                display_name: row.get(2)?,
                introversion: row.get(3)?,
                opted_out: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
            })
        })?;
        let mut members = Vec::new();
        for member in iter {
            members.push(member?);
        }
        Ok(members)
    }

    /// Preserve historical authors while removing absent agents from the live
    /// roster: missing agents become opted out instead of being deleted through
    /// post foreign keys.
    pub fn reconcile_agent_members(&self, active_agent_ids: &HashSet<String>) -> Result<usize> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT member_id FROM members WHERE kind = 'agent'")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut changed = 0;
        for member_id in ids {
            if !active_agent_ids.contains(&member_id) {
                changed += conn.execute(
                    "UPDATE members SET opted_out = 1 WHERE member_id = ?1 AND opted_out = 0",
                    params![member_id],
                )?;
            }
        }
        Ok(changed)
    }

    // Self State
    pub fn upsert_self_state(&self, state: &SelfState) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO self_state (member_id, valence, energy, baseline_valence, baseline_energy, note, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(member_id) DO UPDATE SET
               valence = excluded.valence,
               energy = excluded.energy,
               baseline_valence = excluded.baseline_valence,
               baseline_energy = excluded.baseline_energy,
               note = excluded.note,
               updated_at = excluded.updated_at",
            params![
                state.member_id,
                state.valence,
                state.energy,
                state.baseline_valence,
                state.baseline_energy,
                state.note,
                state.updated_at,
            ],
        ).context("upserting self state")?;
        Ok(())
    }

    pub fn get_self_state(&self, member_id: &str) -> Result<Option<SelfState>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT member_id, valence, energy, baseline_valence, baseline_energy, note, updated_at FROM self_state WHERE member_id = ?1")?;
        let state = stmt
            .query_row(params![member_id], |row| {
                Ok(SelfState {
                    member_id: row.get(0)?,
                    valence: row.get(1)?,
                    energy: row.get(2)?,
                    baseline_valence: row.get(3)?,
                    baseline_energy: row.get(4)?,
                    note: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })
            .optional()?;
        Ok(state)
    }

    pub fn get_all_self_states(&self) -> Result<Vec<SelfState>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT member_id, valence, energy, baseline_valence, baseline_energy, note, updated_at FROM self_state")?;
        let iter = stmt.query_map([], |row| {
            Ok(SelfState {
                member_id: row.get(0)?,
                valence: row.get(1)?,
                energy: row.get(2)?,
                baseline_valence: row.get(3)?,
                baseline_energy: row.get(4)?,
                note: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        let mut states = Vec::new();
        for r in iter {
            states.push(r?);
        }
        Ok(states)
    }

    // Groups
    pub fn create_group(&self, group: &Group, members: &[&str]) -> Result<()> {
        if members.len() > MAX_SOCIAL_GROUP_MEMBERS {
            anyhow::bail!("A group cannot contain more than {MAX_SOCIAL_GROUP_MEMBERS} members.");
        }
        let unique_members = members.iter().copied().collect::<HashSet<_>>();
        if unique_members.len() < 3 || !unique_members.contains(group.created_by.as_str()) {
            anyhow::bail!("A group requires three or more members.");
        }
        if group.name.trim().is_empty() || group.name.chars().count() > 120 {
            anyhow::bail!("A group name must contain 1–120 characters.");
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        for member_id in &unique_members {
            let exists = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM members
                    WHERE member_id = ?1
                      AND (kind = 'operator' OR opted_out = 0)
                 )",
                params![*member_id],
                |row| row.get::<_, i64>(0),
            )? != 0;
            if !exists {
                anyhow::bail!("A group can contain only active enrolled members.");
            }
        }
        tx.execute(
            "INSERT INTO groups (group_id, name, created_by, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                group.group_id,
                group.name,
                group.created_by,
                group.created_at
            ],
        )?;
        for member_id in unique_members {
            tx.execute(
                "INSERT INTO group_members (group_id, member_id) VALUES (?1, ?2)",
                params![group.group_id, member_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_groups_for_member(&self, member_id: &str) -> Result<Vec<Group>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT g.group_id, g.name, g.created_by, g.created_at
             FROM groups g
             JOIN group_members gm ON g.group_id = gm.group_id
             WHERE gm.member_id = ?1
             ORDER BY g.created_at DESC, g.group_id DESC",
        )?;
        let iter = stmt.query_map(params![member_id], |row| {
            Ok(Group {
                group_id: row.get(0)?,
                name: row.get(1)?,
                created_by: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        let mut groups = Vec::new();
        for r in iter {
            groups.push(r?);
        }
        Ok(groups)
    }

    pub fn is_group_member(&self, member_id: &str, group_id: &str) -> Result<bool> {
        let conn = self.connection()?;
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM group_members WHERE group_id = ?1 AND member_id = ?2)",
            params![group_id, member_id],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    pub fn get_group_posts(
        &self,
        member_id: &str,
        group_id: &str,
        limit: usize,
    ) -> Result<Vec<Post>> {
        let conn = self.connection()?;
        // Check membership implicitly by joining group_members
        let mut stmt = conn.prepare(
            "SELECT p.post_id, p.author_id, p.surface, p.group_id, p.post_type, p.body, p.parent_id, p.created_at
             FROM posts p
             JOIN group_members gm ON p.group_id = gm.group_id
             WHERE p.surface = 'group' AND p.group_id = ?1 AND gm.member_id = ?2
             ORDER BY p.created_at DESC, p.post_id DESC
             LIMIT ?3"
        )?;
        let iter = stmt.query_map(params![group_id, member_id, limit as i64], |row| {
            Ok(Post {
                post_id: row.get(0)?,
                author_id: row.get(1)?,
                surface: row.get(2)?,
                group_id: row.get(3)?,
                post_type: row.get(4)?,
                body: row.get(5)?,
                parent_id: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut posts = Vec::new();
        for r in iter {
            posts.push(r?);
        }
        Ok(posts)
    }

    // Posts & Visibility
    pub fn insert_post(&self, post: &Post) -> Result<()> {
        self.insert_post_with_mentions(post).map(|_| ())
    }

    /// Commits the post and every valid, visible `@agent-id` delivery in one
    /// transaction. Unknown handles remain ordinary text; they never create a
    /// phantom recipient or cross a group-visibility boundary.
    pub fn insert_post_with_mentions(&self, post: &Post) -> Result<Vec<String>> {
        self.insert_post_with_delivery_targets(post, &[])
    }

    /// Explicit targets are authoritative roster IDs supplied by a structured
    /// caller. They allow every valid agent identifier to be addressed even
    /// when it cannot be losslessly represented by the conservative free-text
    /// handle lexer. Every target must still appear visibly in the body.
    pub fn insert_post_with_delivery_targets(
        &self,
        post: &Post,
        explicit_targets: &[String],
    ) -> Result<Vec<String>> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let mentioned = insert_post_on(&tx, post, explicit_targets, true)?;
        tx.commit()?;
        if !mentioned.is_empty() {
            self.wake_scope();
        }
        Ok(mentioned)
    }

    /// Commit one worker-authored ambient contribution while keeping automatic
    /// discussion structurally bounded. Ambient replies may target only a
    /// public thought/question root written by another member. An automatic
    /// reply is admitted only while that root has fewer than
    /// `max_direct_replies` direct replies. The check and insert share one
    /// transaction so concurrent workers cannot over-admit a closed thread.
    ///
    /// Ambient posts deliberately do not create mention or reply-notification
    /// deliveries. The next worker candidate sees the newly committed feed
    /// snapshot directly; delivery-triggered replies remain reserved for exact
    /// operator/agent mentions handled by `reply_to_pending_mention`.
    pub fn insert_bounded_ambient_post(
        &self,
        post: &Post,
        max_direct_replies: usize,
        max_thread_age_secs: i64,
        now: &DateTime<Utc>,
    ) -> Result<bool> {
        if post.surface != "feed" || post.group_id.is_some() {
            anyhow::bail!("ambient social posts must target the public feed");
        }
        let active_after = discussion_active_after(now, max_thread_age_secs)?;
        let post_created_at = DateTime::parse_from_rfc3339(&post.created_at)
            .map_err(|_| anyhow::anyhow!("ambient social post timestamp is invalid"))?
            .with_timezone(&Utc);
        if post_created_at < active_after || post_created_at > *now {
            return Ok(false);
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active_root =
            open_public_discussion_root_on(&tx, max_direct_replies, &active_after, now)?;
        if let Some(parent_id) = post.parent_id.as_deref() {
            if post.post_type != "reply" || max_direct_replies == 0 {
                anyhow::bail!("ambient social reply policy is invalid");
            }
            let Some(parent) = active_root.as_ref() else {
                return Ok(false);
            };
            if parent.post_id != parent_id || parent.author_id == post.author_id {
                return Ok(false);
            }
        } else {
            if !matches!(post.post_type.as_str(), "thought" | "question") {
                anyhow::bail!("ambient social roots must be thoughts or questions");
            }
            // Composition can take seconds. Recheck under the same write
            // transaction so a root published while the model was answering
            // cannot be followed by a competing automatic topic.
            if active_root.is_some() {
                return Ok(false);
            }
        }
        insert_post_on(&tx, post, &[], false)?;
        tx.commit()?;
        Ok(true)
    }

    /// Persist an operator-visible post without creating autonomous delivery
    /// state. This is the only valid write mode for a scope omitted from the
    /// configured autonomous scope allowlist.
    pub fn insert_post_without_deliveries(&self, post: &Post) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        insert_post_on(&tx, post, &[], false)?;
        tx.commit()?;
        Ok(())
    }

    fn wake_scope(&self) {
        if let (Some(wake), Some(scope)) = (&self.mention_wake, &self.scope_key) {
            wake.mark_scope(scope.clone());
        }
    }

    /// Returns at most one oldest pending mention per member. This bounds the
    /// worker snapshot while ensuring one noisy author cannot hide another
    /// member's inbox indefinitely.
    pub fn get_pending_mentions(&self, limit: usize) -> Result<Vec<PendingMention>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT m.mentioned_member_id,
                    p.post_id, p.author_id, p.surface, p.group_id, p.post_type,
                    p.body, p.parent_id, p.created_at
             FROM mentions m
             JOIN posts p ON p.post_id = m.post_id
             JOIN members recipient ON recipient.member_id = m.mentioned_member_id
             WHERE m.status = 'pending'
               AND m.delivery_kind = 'explicit_mention'
               AND recipient.kind = 'agent'
               AND recipient.opted_out = 0
               AND m.post_id = (
                 SELECT oldest.post_id
                 FROM mentions oldest
                 WHERE oldest.mentioned_member_id = m.mentioned_member_id
                   AND oldest.status = 'pending'
                   AND oldest.delivery_kind = 'explicit_mention'
                 ORDER BY oldest.created_at ASC, oldest.post_id ASC
                 LIMIT 1
               )
             ORDER BY m.created_at ASC, m.post_id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit.clamp(1, 1_000) as i64], |row| {
            Ok(PendingMention {
                mentioned_member_id: row.get(0)?,
                delivery_kind: "explicit_mention".to_string(),
                post: Post {
                    post_id: row.get(1)?,
                    author_id: row.get(2)?,
                    surface: row.get(3)?,
                    group_id: row.get(4)?,
                    post_type: row.get(5)?,
                    body: row.get(6)?,
                    parent_id: row.get(7)?,
                    created_at: row.get(8)?,
                },
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Informational reply notifications never drive an automatic reply. They
    /// remain durably queryable until a consumer records that they were seen.
    pub fn get_pending_reply_notifications(
        &self,
        member_id: &str,
        limit: usize,
    ) -> Result<Vec<PendingMention>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT m.mentioned_member_id,
                    p.post_id, p.author_id, p.surface, p.group_id, p.post_type,
                    p.body, p.parent_id, p.created_at
             FROM mentions m
             JOIN posts p ON p.post_id = m.post_id
             WHERE m.mentioned_member_id = ?1
               AND m.status = 'pending'
               AND m.delivery_kind = 'reply_notification'
             ORDER BY m.created_at ASC, m.post_id ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![member_id, limit.clamp(1, 100) as i64], |row| {
            Ok(PendingMention {
                mentioned_member_id: row.get(0)?,
                delivery_kind: "reply_notification".to_string(),
                post: Post {
                    post_id: row.get(1)?,
                    author_id: row.get(2)?,
                    surface: row.get(3)?,
                    group_id: row.get(4)?,
                    post_type: row.get(5)?,
                    body: row.get(6)?,
                    parent_id: row.get(7)?,
                    created_at: row.get(8)?,
                },
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn acknowledge_reply_notifications(
        &self,
        member_id: &str,
        post_ids: &[String],
        handled_at: &str,
    ) -> Result<usize> {
        if post_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let mut updated = 0usize;
        for post_id in post_ids.iter().take(100) {
            updated += tx.execute(
                "UPDATE mentions
                 SET status = 'seen', handled_at = ?3
                 WHERE post_id = ?1 AND mentioned_member_id = ?2
                   AND delivery_kind = 'reply_notification' AND status = 'pending'",
                params![post_id, member_id, handled_at],
            )?;
        }
        tx.commit()?;
        Ok(updated)
    }

    pub fn has_pending_mentions(&self) -> Result<bool> {
        let conn = self.connection()?;
        Ok(conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM mentions
                WHERE status = 'pending' AND delivery_kind = 'explicit_mention'
             )",
            [],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    /// Records that an agent consciously declined a mention. Provider errors,
    /// busy agents, and budget exhaustion do not acknowledge it, so recovery
    /// can retry after the blocking condition clears.
    pub fn pass_pending_mention(
        &self,
        post_id: &str,
        mentioned_member_id: &str,
        handled_at: &str,
    ) -> Result<bool> {
        let conn = self.connection()?;
        Ok(conn.execute(
            "UPDATE mentions
             SET status = 'passed', handled_at = ?3
             WHERE post_id = ?1 AND mentioned_member_id = ?2
               AND delivery_kind = 'explicit_mention' AND status = 'pending'",
            params![post_id, mentioned_member_id, handled_at],
        )? == 1)
    }

    /// Atomically publishes a response and acknowledges the triggering
    /// mention. A crash can therefore leave either the pending mention or the
    /// completed reply, never an unacknowledged duplicate-producing gap.
    pub fn reply_to_pending_mention(
        &self,
        mention_post_id: &str,
        mentioned_member_id: &str,
        reply: &Post,
        handled_at: &str,
    ) -> Result<Option<Vec<String>>> {
        self.reply_to_pending_mention_with_targets(
            mention_post_id,
            mentioned_member_id,
            reply,
            handled_at,
            &[],
        )
    }

    pub fn reply_to_pending_mention_with_targets(
        &self,
        mention_post_id: &str,
        mentioned_member_id: &str,
        reply: &Post,
        handled_at: &str,
        explicit_targets: &[String],
    ) -> Result<Option<Vec<String>>> {
        if reply.author_id != mentioned_member_id
            || reply.post_type != "reply"
            || reply.parent_id.as_deref() != Some(mention_post_id)
        {
            anyhow::bail!("social mention reply does not match its pending delivery");
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let pending = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM mentions
                WHERE post_id = ?1 AND mentioned_member_id = ?2
                  AND delivery_kind = 'explicit_mention' AND status = 'pending'
             )",
            params![mention_post_id, mentioned_member_id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !pending {
            return Ok(None);
        }
        let mentioned = insert_post_on(&tx, reply, explicit_targets, true)?;
        let updated = tx.execute(
            "UPDATE mentions
             SET status = 'replied', handled_at = ?3, response_post_id = ?4
             WHERE post_id = ?1 AND mentioned_member_id = ?2
               AND delivery_kind = 'explicit_mention' AND status = 'pending'",
            params![
                mention_post_id,
                mentioned_member_id,
                handled_at,
                reply.post_id
            ],
        )?;
        if updated != 1 {
            return Ok(None);
        }
        tx.commit()?;
        if !mentioned.is_empty() {
            self.wake_scope();
        }
        Ok(Some(mentioned))
    }

    pub fn has_pending_mention(&self, post_id: &str, mentioned_member_id: &str) -> Result<bool> {
        let conn = self.connection()?;
        Ok(conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM mentions
                WHERE post_id = ?1 AND mentioned_member_id = ?2
                  AND delivery_kind = 'explicit_mention' AND status = 'pending'
             )",
            params![post_id, mentioned_member_id],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    /* Post validation and persistence are shared by ordinary posts and the
     * atomic mention-reply path below. */
    fn validate_post_shape(post: &Post) -> Result<()> {
        match (post.surface.as_str(), post.group_id.as_deref()) {
            ("feed", None) | ("group", Some(_)) => {},
            _ => anyhow::bail!("invalid social post surface/group binding"),
        }
        if post.body.trim().is_empty() {
            anyhow::bail!("social post body must not be empty");
        }
        if post.body.chars().count() > MAX_SOCIAL_POST_CHARS {
            anyhow::bail!("social post body exceeds the absolute storage limit");
        }
        if contains_secret_shaped_content(&post.body) {
            anyhow::bail!("social post body resembles a credential or secret");
        }
        if !matches!(
            post.post_type.as_str(),
            "thought" | "reply" | "question" | "link"
        ) {
            anyhow::bail!("invalid social post type");
        }
        Ok(())
    }

    pub fn visible_posts_for(&self, member_id: &str, limit: usize) -> Result<Vec<Post>> {
        // Enforces:
        // Feed = all members.
        // Group = group_members only.
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT post_id, author_id, surface, group_id, post_type, body, parent_id, created_at
             FROM posts
             WHERE surface = 'feed'
                OR (surface = 'group' AND group_id IN (SELECT group_id FROM group_members WHERE member_id = ?1))
             ORDER BY created_at DESC, post_id DESC
             LIMIT ?2"
        )?;
        let iter = stmt.query_map(params![member_id, limit as i64], |row| {
            Ok(Post {
                post_id: row.get(0)?,
                author_id: row.get(1)?,
                surface: row.get(2)?,
                group_id: row.get(3)?,
                post_type: row.get(4)?,
                body: row.get(5)?,
                parent_id: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut posts = Vec::new();
        for r in iter {
            posts.push(r?);
        }
        Ok(posts)
    }

    pub fn get_feed_posts(&self, limit: usize) -> Result<Vec<Post>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT post_id, author_id, surface, group_id, post_type, body, parent_id, created_at
             FROM posts
             WHERE surface = 'feed'
             ORDER BY created_at DESC, post_id DESC
             LIMIT ?1",
        )?;
        let iter = stmt.query_map(params![limit as i64], |row| {
            Ok(Post {
                post_id: row.get(0)?,
                author_id: row.get(1)?,
                surface: row.get(2)?,
                group_id: row.get(3)?,
                post_type: row.get(4)?,
                body: row.get(5)?,
                parent_id: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut posts = Vec::new();
        for r in iter {
            posts.push(r?);
        }
        Ok(posts)
    }

    pub fn direct_reply_count(&self, parent_id: &str) -> Result<usize> {
        let conn = self.connection()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE parent_id = ?1",
            params![parent_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn open_public_discussion_root(
        &self,
        max_direct_replies: usize,
        max_thread_age_secs: i64,
        now: &DateTime<Utc>,
    ) -> Result<Option<Post>> {
        let active_after = discussion_active_after(now, max_thread_age_secs)?;
        let conn = self.connection()?;
        open_public_discussion_root_on(&conn, max_direct_replies, &active_after, now)
    }

    pub fn get_feed_posts_before(
        &self,
        limit: usize,
        before: Option<(&str, &str)>,
    ) -> Result<Vec<Post>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT post_id, author_id, surface, group_id, post_type, body, parent_id, created_at
             FROM posts
             WHERE surface = 'feed'
               AND (?1 IS NULL
                    OR created_at < ?1
                    OR (created_at = ?1 AND post_id < ?2))
             ORDER BY created_at DESC, post_id DESC
             LIMIT ?3",
        )?;
        let before_created_at = before.map(|value| value.0);
        let before_post_id = before.map(|value| value.1);
        let iter = stmt.query_map(
            params![before_created_at, before_post_id, limit as i64],
            |row| {
                Ok(Post {
                    post_id: row.get(0)?,
                    author_id: row.get(1)?,
                    surface: row.get(2)?,
                    group_id: row.get(3)?,
                    post_type: row.get(4)?,
                    body: row.get(5)?,
                    parent_id: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        )?;
        Ok(iter.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn latest_post_at_for(&self, member_id: &str) -> Result<Option<String>> {
        let conn = self.connection()?;
        Ok(conn
            .query_row(
                "SELECT created_at FROM posts WHERE author_id = ?1 ORDER BY created_at DESC LIMIT 1",
                params![member_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn insert_reaction(&self, reaction: &Reaction) -> Result<()> {
        let conn = self.connection()?;
        let inserted = conn.execute(
            "INSERT OR REPLACE INTO reactions (post_id, member_id, emoji, created_at)
             SELECT p.post_id, ?2, ?3, ?4
             FROM posts p
             JOIN members reactor ON reactor.member_id = ?2
             WHERE p.post_id = ?1
               AND (reactor.kind = 'operator' OR reactor.opted_out = 0)
               AND (p.surface = 'feed'
                    OR EXISTS (
                        SELECT 1 FROM group_members gm
                        WHERE gm.group_id = p.group_id AND gm.member_id = ?2
                    ))",
            params![
                reaction.post_id,
                reaction.member_id,
                reaction.emoji,
                reaction.created_at
            ],
        )?;
        if inserted != 1 {
            anyhow::bail!("social post is absent or not visible to the reacting member");
        }
        Ok(())
    }

    pub fn is_post_visible_to(&self, member_id: &str, post_id: &str) -> Result<bool> {
        let conn = self.connection()?;
        Ok(conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM posts p
                WHERE p.post_id = ?1
                  AND (p.surface = 'feed'
                       OR EXISTS (
                           SELECT 1 FROM group_members gm
                           WHERE gm.group_id = p.group_id AND gm.member_id = ?2
                       ))
            )",
            params![post_id, member_id],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    pub fn get_reactions_for_post(&self, post_id: &str) -> Result<Vec<Reaction>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT post_id, member_id, emoji, created_at FROM reactions WHERE post_id = ?1 ORDER BY created_at ASC"
        )?;
        let iter = stmt.query_map(params![post_id], |row| {
            Ok(Reaction {
                post_id: row.get(0)?,
                member_id: row.get(1)?,
                emoji: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        let mut reactions = Vec::new();
        for r in iter {
            reactions.push(r?);
        }
        Ok(reactions)
    }

    pub fn get_reactions_for_posts(
        &self,
        post_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<Reaction>>> {
        if post_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let conn = self.connection()?;

        let placeholders = post_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!("SELECT post_id, member_id, emoji, created_at FROM reactions WHERE post_id IN ({}) ORDER BY created_at ASC", placeholders);
        let mut stmt = conn.prepare(&query)?;

        let iter = stmt.query_map(rusqlite::params_from_iter(post_ids), |row| {
            Ok(Reaction {
                post_id: row.get(0)?,
                member_id: row.get(1)?,
                emoji: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;

        let mut reactions_map: std::collections::HashMap<String, Vec<Reaction>> =
            std::collections::HashMap::new();
        for r in iter {
            let reaction = r?;
            reactions_map
                .entry(reaction.post_id.clone())
                .or_default()
                .push(reaction);
        }
        Ok(reactions_map)
    }

    pub fn delete_reaction(&self, post_id: &str, member_id: &str, emoji: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "DELETE FROM reactions WHERE post_id = ?1 AND member_id = ?2 AND emoji = ?3",
            params![post_id, member_id, emoji],
        )?;
        Ok(())
    }

    pub fn validate_reply_parent(&self, post: &Post) -> Result<()> {
        let conn = self.connection()?;
        Self::validate_post_shape(post)?;
        match (post.post_type.as_str(), post.parent_id.as_deref()) {
            ("reply", Some(parent_id)) => {
                let parent: Option<(String, Option<String>)> = conn
                    .query_row(
                        "SELECT surface, group_id FROM posts WHERE post_id = ?1",
                        params![parent_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if !matches!(
                    parent.as_ref(),
                    Some((surface, group_id))
                        if surface == &post.surface && group_id == &post.group_id
                ) {
                    anyhow::bail!("social reply parent is absent or outside the target surface");
                }
            },
            ("reply", None) => anyhow::bail!("social replies require a parent post"),
            (_, Some(_)) => anyhow::bail!("only social replies may reference a parent post"),
            (_, None) => {},
        }
        Ok(())
    }
}

fn discussion_active_after(now: &DateTime<Utc>, max_thread_age_secs: i64) -> Result<DateTime<Utc>> {
    if max_thread_age_secs <= 0 {
        anyhow::bail!("ambient discussion age must be positive");
    }
    let max_age = ChronoDuration::try_seconds(max_thread_age_secs)
        .ok_or_else(|| anyhow::anyhow!("ambient discussion age is out of range"))?;
    now.checked_sub_signed(max_age)
        .ok_or_else(|| anyhow::anyhow!("ambient discussion window is out of range"))
}

fn open_public_discussion_root_on(
    conn: &Connection,
    max_direct_replies: usize,
    active_after: &DateTime<Utc>,
    active_before: &DateTime<Utc>,
) -> Result<Option<Post>> {
    if max_direct_replies == 0 {
        anyhow::bail!("ambient direct-reply limit must be positive");
    }
    let max_direct_replies = i64::try_from(max_direct_replies).unwrap_or(i64::MAX);
    conn.query_row(
        "SELECT root.post_id, root.author_id, root.surface, root.group_id,
                root.post_type, root.body, root.parent_id, root.created_at
         FROM posts root
         WHERE root.surface = 'feed'
           AND root.group_id IS NULL
           AND root.parent_id IS NULL
           AND root.post_type IN ('thought', 'question')
           AND julianday(root.created_at) >= julianday(?1)
           AND julianday(root.created_at) <= julianday(?2)
           AND (SELECT COUNT(*) FROM posts reply WHERE reply.parent_id = root.post_id) < ?3
         ORDER BY julianday(root.created_at) DESC, root.post_id DESC
         LIMIT 1",
        params![
            active_after.to_rfc3339(),
            active_before.to_rfc3339(),
            max_direct_replies,
        ],
        |row| {
            Ok(Post {
                post_id: row.get(0)?,
                author_id: row.get(1)?,
                surface: row.get(2)?,
                group_id: row.get(3)?,
                post_type: row.get(4)?,
                body: row.get(5)?,
                parent_id: row.get(6)?,
                created_at: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn insert_post_on(
    conn: &Connection,
    post: &Post,
    explicit_targets: &[String],
    deliveries_enabled: bool,
) -> Result<Vec<String>> {
    SocialStore::validate_post_shape(post)?;
    if explicit_targets.len() > MAX_MENTIONS_PER_POST {
        anyhow::bail!("explicit social mention target count exceeds the per-post limit");
    }
    let author_state = conn
        .query_row(
            "SELECT kind, opted_out FROM members WHERE member_id = ?1",
            params![post.author_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()?;
    if author_state.is_some_and(|(kind, opted_out)| kind != "operator" && opted_out) {
        anyhow::bail!("social post author is not an active roster member");
    }
    if let Some(group_id) = post.group_id.as_deref() {
        let is_member = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM group_members WHERE group_id = ?1 AND member_id = ?2)",
            params![group_id, post.author_id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !is_member {
            anyhow::bail!("social post author is not a member of the target group");
        }
    }
    match (post.post_type.as_str(), post.parent_id.as_deref()) {
        ("reply", Some(parent_id)) => {
            let parent: Option<(String, Option<String>)> = conn
                .query_row(
                    "SELECT surface, group_id FROM posts WHERE post_id = ?1",
                    params![parent_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if !matches!(
                parent.as_ref(),
                Some((surface, group_id))
                    if surface == &post.surface && group_id == &post.group_id
            ) {
                anyhow::bail!("social reply parent is absent or outside the target surface");
            }
        },
        ("reply", None) => anyhow::bail!("social replies require a parent post"),
        (_, Some(_)) => anyhow::bail!("only social replies may reference a parent post"),
        (_, None) => {},
    }
    conn.execute(
        "INSERT INTO posts (post_id, author_id, surface, group_id, post_type, body, parent_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            post.post_id,
            post.author_id,
            post.surface,
            post.group_id,
            post.post_type,
            post.body,
            post.parent_id,
            post.created_at,
        ],
    )?;

    let mut mentioned = Vec::new();
    // The configured social profiles may use a remote provider, so only the
    // public feed participates in autonomous mention delivery. Private-group
    // text is never projected into that worker prompt.
    if post.surface != "feed" || !deliveries_enabled {
        return Ok(mentioned);
    }
    let mut candidates = Vec::new();
    let mut seen_candidates = HashSet::new();
    for member_id in explicit_targets {
        if !body_contains_exact_mention(&post.body, member_id) {
            anyhow::bail!("explicit social mention target is not visible in the post body");
        }
        if seen_candidates.insert(member_id.clone()) {
            candidates.push((member_id.clone(), true));
        }
    }
    for handle in extract_mention_handles(&post.body) {
        if seen_candidates.insert(handle.clone()) {
            candidates.push((handle, false));
        }
    }

    for (handle, explicit) in candidates {
        if mentioned.len() >= MAX_MENTIONS_PER_POST {
            break;
        }
        if handle == post.author_id {
            continue;
        }
        let visible_recipient = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM members member
                WHERE member.member_id = ?1
                  AND member.kind = 'agent'
                  AND member.opted_out = 0
                  AND (?2 = 'feed' OR EXISTS (
                    SELECT 1 FROM group_members gm
                    WHERE gm.group_id = ?3 AND gm.member_id = member.member_id
                  ))
             )",
            params![handle.as_str(), post.surface, post.group_id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !visible_recipient {
            if explicit {
                anyhow::bail!("explicit social mention target is not an eligible roster member");
            }
            continue;
        }
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO mentions
             (post_id, mentioned_member_id, delivery_kind, status, created_at)
             VALUES (?1, ?2, 'explicit_mention', 'pending', ?3)",
            params![post.post_id, handle.as_str(), post.created_at],
        )?;
        if inserted == 1 {
            mentioned.push(handle);
        }
    }

    // A public-feed reply leaves a durable notification for the parent author.
    // It is deliberately informational rather than actionable: only an
    // explicit mention may trigger an autonomous reply, preventing
    // reply-notification loops. Private-group text stays outside the
    // potentially remote social worker. If the body explicitly tagged the
    // parent, that stronger delivery already owns this post/recipient key.
    if post.post_type == "reply" {
        if let Some(parent_id) = post.parent_id.as_deref() {
            let parent_author: Option<String> = conn
                .query_row(
                    "SELECT parent.author_id
                 FROM posts parent
                 JOIN members author ON author.member_id = parent.author_id
                 WHERE parent.post_id = ?1
                   AND author.kind = 'agent' AND author.opted_out = 0",
                    params![parent_id],
                    |row| row.get(0),
                )
                .optional()?;
            if parent_author
                .as_deref()
                .is_some_and(|author| author != post.author_id.as_str())
            {
                conn.execute(
                    "INSERT OR IGNORE INTO mentions
                     (post_id, mentioned_member_id, delivery_kind, status, created_at)
                     VALUES (?1, ?2, 'reply_notification', 'pending', ?3)",
                    params![post.post_id, parent_author, post.created_at],
                )?;
            }
        }
    }
    Ok(mentioned)
}

/// Extract stable social handles without treating the `@` inside an email
/// address as a mention. This is deliberately a conservative free-text fast
/// path; valid agent IDs outside this vocabulary arrive through the structured
/// target list and still have to match exact visible text.
/// `@handle` tokens typed in a post body.
///
/// Public for the same reason as [`MAX_SOCIAL_GROUP_MEMBERS`]: mention fan-out
/// moved to the HTTP surface with the corpus, and reimplementing this by eye
/// there would quietly change which handles notify.
pub fn extract_mention_handles(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut seen = HashSet::new();
    let mut handles = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'@' {
            index += 1;
            continue;
        }
        if index > 0 && is_mention_handle_byte(bytes[index - 1]) {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < bytes.len() && is_mention_handle_byte(bytes[end]) {
            end += 1;
        }
        if end > start {
            let handle = value[start..end].to_string();
            if seen.insert(handle.clone()) {
                handles.push(handle);
            }
        }
        index = end.max(index + 1);
    }
    handles
}

fn is_mention_handle_byte(value: u8) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-' | b'.')
}

/// Whether a post body actually names this member.
///
/// The rule it enforces is not cosmetic: a delivery to someone the reader
/// cannot see named in the text is a notification with no visible cause.
pub fn body_contains_exact_mention(body: &str, member_id: &str) -> bool {
    let needle = format!("@{member_id}");
    body.match_indices(&needle).any(|(start, _)| {
        let previous_is_handle = start
            .checked_sub(1)
            .and_then(|index| body.as_bytes().get(index))
            .is_some_and(|byte| is_mention_handle_byte(*byte));
        let end = start + needle.len();
        let next_is_handle = body
            .as_bytes()
            .get(end)
            .is_some_and(|byte| is_mention_handle_byte(*byte));
        !previous_is_handle && !next_is_handle
    })
}

fn migrate_mention_schema(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare("PRAGMA table_info(mentions)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    drop(stmt);
    if !columns.contains("delivery_kind") {
        tx.execute(
            "ALTER TABLE mentions ADD COLUMN delivery_kind TEXT NOT NULL DEFAULT 'explicit_mention'",
            [],
        )?;
    }
    if !columns.contains("created_at") {
        tx.execute(
            "ALTER TABLE mentions ADD COLUMN created_at TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    tx.execute(
        "UPDATE mentions
         SET created_at = COALESCE((SELECT posts.created_at FROM posts WHERE posts.post_id = mentions.post_id), '')
         WHERE created_at = ''",
        [],
    )?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS mentions_recipient_status_created
         ON mentions(status, delivery_kind, mentioned_member_id, created_at, post_id)",
        [],
    )?;
    // Keep indexes that reference migrated columns behind the additive column
    // upgrades above. Putting these in BOOTSTRAP_DDL would make an older
    // mentions table fail before the migration had a chance to add them.
    tx.execute(
        "CREATE INDEX IF NOT EXISTS mentions_pending_delivery_created
         ON mentions(delivery_kind, status, created_at, post_id, mentioned_member_id)",
        [],
    )?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS mentions_response_post
         ON mentions(response_post_id)",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Town Square corpus export readers (queue item 6, slice 3)
//
// Free functions over a borrowed connection so `export_corpus` can run all
// eight inside one transaction. Each is ordered by its primary key, which is
// what makes two exports over an unchanged corpus byte-identical.
// ---------------------------------------------------------------------------

/// Every table the migration moves, read at one instant.
#[derive(Debug, Clone, PartialEq)]
pub struct SocialCorpusExport {
    pub members: Vec<Member>,
    pub self_states: Vec<SelfState>,
    pub groups: Vec<Group>,
    pub memberships: Vec<GroupMembershipRow>,
    pub posts: Vec<Post>,
    pub mentions: Vec<MentionRow>,
    pub reactions: Vec<Reaction>,
    pub operator_policy: Option<OperatorPolicyRow>,
}

impl SocialCorpusExport {
    fn enforce_row_bounds(&self) -> Result<()> {
        for (table, rows) in [
            ("members", self.members.len()),
            ("self_state", self.self_states.len()),
            ("groups", self.groups.len()),
            ("group_members", self.memberships.len()),
            ("posts", self.posts.len()),
            ("mentions", self.mentions.len()),
            ("reactions", self.reactions.len()),
        ] {
            if rows > SocialStore::MIGRATION_MAX_ROWS_PER_TABLE {
                anyhow::bail!(
                    "social table `{table}` holds {rows} rows, above the {} the migration can \
                     read back and therefore prove; it will not move a corpus it cannot verify",
                    SocialStore::MIGRATION_MAX_ROWS_PER_TABLE
                );
            }
        }
        Ok(())
    }
}

fn export_members(conn: &Connection) -> Result<Vec<Member>> {
    let mut stmt = conn.prepare(
        "SELECT member_id, kind, display_name, introversion, opted_out, created_at
         FROM members ORDER BY member_id",
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(Member {
            member_id: row.get(0)?,
            kind: row.get(1)?,
            display_name: row.get(2)?,
            introversion: row.get(3)?,
            opted_out: row.get::<_, i64>(4)? != 0,
            created_at: row.get(5)?,
        })
    })?;
    let mut rows = Vec::new();
    for row in iter {
        rows.push(row?);
    }
    Ok(rows)
}

fn export_self_states(conn: &Connection) -> Result<Vec<SelfState>> {
    let mut stmt = conn.prepare(
        "SELECT member_id, valence, energy, baseline_valence, baseline_energy, note, updated_at
         FROM self_state ORDER BY member_id",
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(SelfState {
            member_id: row.get(0)?,
            valence: row.get(1)?,
            energy: row.get(2)?,
            baseline_valence: row.get(3)?,
            baseline_energy: row.get(4)?,
            note: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;
    let mut rows = Vec::new();
    for row in iter {
        rows.push(row?);
    }
    Ok(rows)
}

fn export_groups(conn: &Connection) -> Result<Vec<Group>> {
    let mut stmt = conn
        .prepare("SELECT group_id, name, created_by, created_at FROM groups ORDER BY group_id")?;
    let iter = stmt.query_map([], |row| {
        Ok(Group {
            group_id: row.get(0)?,
            name: row.get(1)?,
            created_by: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    let mut rows = Vec::new();
    for row in iter {
        rows.push(row?);
    }
    Ok(rows)
}

fn export_group_memberships(conn: &Connection) -> Result<Vec<GroupMembershipRow>> {
    let mut stmt =
        conn.prepare("SELECT group_id, member_id FROM group_members ORDER BY group_id, member_id")?;
    let iter = stmt.query_map([], |row| {
        Ok(GroupMembershipRow {
            group_id: row.get(0)?,
            member_id: row.get(1)?,
        })
    })?;
    let mut rows = Vec::new();
    for row in iter {
        rows.push(row?);
    }
    Ok(rows)
}

fn export_posts(conn: &Connection) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare(
        "SELECT post_id, author_id, surface, group_id, post_type, body, parent_id, created_at
         FROM posts ORDER BY post_id",
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(Post {
            post_id: row.get(0)?,
            author_id: row.get(1)?,
            surface: row.get(2)?,
            group_id: row.get(3)?,
            post_type: row.get(4)?,
            body: row.get(5)?,
            parent_id: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;
    let mut rows = Vec::new();
    for row in iter {
        rows.push(row?);
    }
    Ok(rows)
}

fn export_mentions(conn: &Connection) -> Result<Vec<MentionRow>> {
    let mut stmt = conn.prepare(
        "SELECT post_id, mentioned_member_id, delivery_kind, status, created_at,
                handled_at, response_post_id
         FROM mentions ORDER BY post_id, mentioned_member_id",
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(MentionRow {
            post_id: row.get(0)?,
            mentioned_member_id: row.get(1)?,
            delivery_kind: row.get(2)?,
            status: row.get(3)?,
            created_at: row.get(4)?,
            handled_at: row.get(5)?,
            response_post_id: row.get(6)?,
        })
    })?;
    let mut rows = Vec::new();
    for row in iter {
        rows.push(row?);
    }
    Ok(rows)
}

fn export_reactions(conn: &Connection) -> Result<Vec<Reaction>> {
    let mut stmt = conn.prepare(
        "SELECT post_id, member_id, emoji, created_at
         FROM reactions ORDER BY post_id, member_id, emoji",
    )?;
    let iter = stmt.query_map([], |row| {
        Ok(Reaction {
            post_id: row.get(0)?,
            member_id: row.get(1)?,
            emoji: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    let mut rows = Vec::new();
    for row in iter {
        rows.push(row?);
    }
    Ok(rows)
}

/// The operator policy row, if the engine ever wrote one.
///
/// `None` is a real answer rather than an error: a square whose operator never
/// touched autonomy has no row, and the migration writes the package's own
/// defaults in that case.
fn export_operator_policy(conn: &Connection) -> Result<Option<OperatorPolicyRow>> {
    let row = conn
        .query_row(
            "SELECT autonomous_enabled, updated_at FROM operator_policy WHERE id = 1",
            [],
            |row| {
                Ok(OperatorPolicyRow {
                    autonomous_enabled: row.get::<_, i64>(0)? != 0,
                    updated_at: row.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn operator_policy_defaults_off_and_persists() {
        let store = SocialStore::open_in_temp().expect("should open");
        assert_eq!(store.operator_policy().unwrap(), (false, None));
        assert!(!store.autonomous_enabled().unwrap());
        store
            .set_autonomous_enabled(true, "2026-08-21T10:00:00Z")
            .unwrap();
        assert_eq!(
            store.operator_policy().unwrap(),
            (true, Some("2026-08-21T10:00:00Z".to_string()))
        );
        store
            .set_autonomous_enabled(false, "2026-08-21T10:01:00Z")
            .unwrap();
        assert_eq!(
            store.operator_policy().unwrap(),
            (false, Some("2026-08-21T10:01:00Z".to_string()))
        );
    }

    #[test]
    fn test_store_opens_and_migrates_idempotently() {
        let store = SocialStore::open_in_temp().expect("should open");
        // opening and running DDL again should be idempotent
        let conn = store.conn.lock().unwrap();
        conn.execute_batch(BOOTSTRAP_DDL)
            .expect("should be idempotent");
        for index in [
            "posts_surface_created_id",
            "posts_author_created_id",
            "posts_created_id",
            "posts_group_created_id",
            "mentions_recipient_status_created",
            "mentions_pending_delivery_created",
            "mentions_response_post",
            "group_members_member_group",
        ] {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![index],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                1,
                "social query index {index} must remain in the initialized schema"
            );
        }
    }

    #[test]
    fn legacy_mention_schema_upgrades_before_the_new_covering_index_is_created() {
        let root = tempfile::tempdir().unwrap();
        let db_path = root.path().join("social.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE members (
                  member_id TEXT PRIMARY KEY, kind TEXT NOT NULL,
                  display_name TEXT NOT NULL, introversion REAL NOT NULL,
                  opted_out INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL
                );
                CREATE TABLE posts (
                  post_id TEXT PRIMARY KEY, author_id TEXT NOT NULL,
                  surface TEXT NOT NULL, group_id TEXT, post_type TEXT NOT NULL,
                  body TEXT NOT NULL, parent_id TEXT, created_at TEXT NOT NULL
                );
                CREATE TABLE mentions (
                  post_id TEXT NOT NULL, mentioned_member_id TEXT NOT NULL,
                  status TEXT NOT NULL DEFAULT 'pending', handled_at TEXT,
                  response_post_id TEXT,
                  PRIMARY KEY (post_id, mentioned_member_id)
                );
                INSERT INTO members VALUES
                  ('operator', 'operator', 'Operator', 0.5, 0, '2026-08-15T23:59:00Z'),
                  ('agent-a', 'agent', 'Agent A', 0.5, 0, '2026-08-15T23:59:00Z');
                INSERT INTO posts VALUES
                  ('legacy-post', 'operator', 'feed', NULL, 'question',
                   '@agent-a legacy delivery', NULL, '2026-08-16T00:00:00Z');
                INSERT INTO mentions
                  (post_id, mentioned_member_id, status, handled_at, response_post_id)
                VALUES ('legacy-post', 'agent-a', 'pending', NULL, NULL);
                "#,
            )
            .unwrap();
        }

        let store = SocialStore::open(root.path()).expect("legacy store should migrate");
        let pending = store.get_pending_mentions(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].delivery_kind, "explicit_mention");
        assert_eq!(pending[0].post.created_at, "2026-08-16T00:00:00Z");
        let conn = store.conn.lock().unwrap();
        for index in [
            "mentions_recipient_status_created",
            "mentions_pending_delivery_created",
            "mentions_response_post",
        ] {
            let index_sql: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![index],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(index_sql.contains("delivery_kind") || index == "mentions_response_post");
        }
    }

    #[cfg(unix)]
    #[test]
    fn social_store_rejects_a_symlinked_database_boundary() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), root.path().join("social.db")).unwrap();
        let error = match SocialStore::open(root.path()) {
            Ok(_) => panic!("a scoped social database must not follow a symlink"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("symbolic link"));
    }

    #[test]
    fn poisoned_connection_degrades_to_an_error_instead_of_panicking_callers() {
        let store = SocialStore::open_in_temp().unwrap();
        let poison_target = store.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _connection = poison_target.conn.lock().unwrap();
            panic!("poison the social connection for the regression fixture");
        }));

        let error = store
            .get_all_members()
            .expect_err("a poisoned connection must be an ordinary store error");
        assert!(error.to_string().contains("connection lock poisoned"));
    }

    #[test]
    fn test_visibility_helper_and_group_rules() {
        let store = SocialStore::open_in_temp().unwrap();

        store
            .upsert_member(&Member {
                member_id: "agent_1".into(),
                kind: "agent".into(),
                display_name: "Agent One".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        store
            .upsert_member(&Member {
                member_id: "operator".into(),
                kind: "operator".into(),
                display_name: "Operator".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();

        let group = Group {
            group_id: "g1".into(),
            name: "Small Group".into(),
            created_by: "agent_1".into(),
            created_at: "now".into(),
        };
        // Should reject < 3 members
        assert!(store
            .create_group(&group, &["agent_1", "operator"])
            .is_err());
        assert!(store
            .create_group(&group, &["agent_1", "operator", "missing-agent"])
            .expect_err("unknown roster members must be a client-visible group error")
            .to_string()
            .contains("enrolled members"));
        store
            .upsert_member(&Member {
                member_id: "retired-agent".into(),
                kind: "agent".into(),
                display_name: "Retired Agent".into(),
                introversion: 0.5,
                opted_out: true,
                created_at: "now".into(),
            })
            .unwrap();
        assert!(store
            .create_group(&group, &["agent_1", "operator", "retired-agent"])
            .expect_err("historical roster rows must not remain inviteable")
            .to_string()
            .contains("active enrolled members"));

        let mut oversized_members = vec!["operator".to_string(), "agent_1".to_string()];
        for index in 0..MAX_SOCIAL_GROUP_MEMBERS {
            let member_id = format!("member-{index}");
            store
                .upsert_member(&Member {
                    member_id: member_id.clone(),
                    kind: "agent".into(),
                    display_name: member_id.clone(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: "now".into(),
                })
                .unwrap();
            oversized_members.push(member_id);
        }
        let oversized_refs = oversized_members
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert!(store
            .create_group(&group, &oversized_refs)
            .expect_err("group fanout must have an absolute storage boundary")
            .to_string()
            .contains("more than"));
    }

    #[test]
    fn storage_boundary_rejects_an_oversized_post_from_any_writer() {
        let store = SocialStore::open_in_temp().unwrap();
        store
            .upsert_member(&Member {
                member_id: "operator".into(),
                kind: "operator".into(),
                display_name: "Operator".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        let error = store
            .insert_post(&Post {
                post_id: "oversized".into(),
                author_id: "operator".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "thought".into(),
                body: "x".repeat(MAX_SOCIAL_POST_CHARS + 1),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .expect_err("the store is the final size boundary for every writer");
        assert!(error.to_string().contains("absolute storage limit"));
        assert!(store.get_feed_posts(10).unwrap().is_empty());
    }

    #[test]
    fn public_mentions_are_durable_and_reply_acknowledgement_is_atomic() {
        let store = SocialStore::open_in_temp().unwrap();
        for id in ["operator", "agent-a", "agent-b"] {
            store
                .upsert_member(&Member {
                    member_id: id.into(),
                    kind: if id == "operator" {
                        "operator"
                    } else {
                        "agent"
                    }
                    .into(),
                    display_name: id.into(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: "2026-08-16T00:00:00Z".into(),
                })
                .unwrap();
        }
        let mentioned = store
            .insert_post_with_mentions(&Post {
                post_id: "question-1".into(),
                author_id: "operator".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "question".into(),
                body: "@agent-a can you review this? mail agent-b@example.com".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:01Z".into(),
            })
            .unwrap();
        assert_eq!(mentioned, vec!["agent-a"]);
        let pending = store.get_pending_mentions(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].mentioned_member_id, "agent-a");
        assert_eq!(pending[0].post.post_id, "question-1");

        let reply = Post {
            post_id: "reply-1".into(),
            author_id: "agent-a".into(),
            surface: "feed".into(),
            group_id: None,
            post_type: "reply".into(),
            body: "Yes — the bounded approach looks sound.".into(),
            parent_id: Some("question-1".into()),
            created_at: "2026-08-16T00:00:02Z".into(),
        };
        store
            .reply_to_pending_mention("question-1", "agent-a", &reply, "2026-08-16T00:00:02Z")
            .unwrap()
            .expect("pending mention commits its reply");
        assert!(store.get_pending_mentions(10).unwrap().is_empty());
        assert_eq!(store.get_feed_posts(10).unwrap()[0], reply);
        assert!(store
            .reply_to_pending_mention(
                "question-1",
                "agent-a",
                &Post {
                    post_id: "duplicate".into(),
                    ..reply.clone()
                },
                "2026-08-16T00:00:03Z",
            )
            .unwrap()
            .is_none());
        assert_eq!(store.get_feed_posts(10).unwrap().len(), 2);

        store
            .insert_post(&Post {
                post_id: "agent-thought".into(),
                author_id: "agent-b".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "thought".into(),
                body: "A useful thought from B".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:04Z".into(),
            })
            .unwrap();
        store
            .insert_post(&Post {
                post_id: "operator-reply".into(),
                author_id: "operator".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "reply".into(),
                body: "That is useful.".into(),
                parent_id: Some("agent-thought".into()),
                created_at: "2026-08-16T00:00:05Z".into(),
            })
            .unwrap();
        let notifications = store
            .get_pending_reply_notifications("agent-b", 10)
            .unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].delivery_kind, "reply_notification");
        assert_eq!(notifications[0].post.post_id, "operator-reply");
        assert_eq!(
            store
                .pending_reply_notification_counts()
                .unwrap()
                .get("agent-b"),
            Some(&1)
        );
        assert!(
            !store
                .has_pending_mention("operator-reply", "agent-b")
                .unwrap(),
            "informational reply notifications must never enter the actionable mention path"
        );
        assert_eq!(
            store
                .acknowledge_reply_notifications(
                    "agent-b",
                    &["operator-reply".to_string()],
                    "2026-08-16T00:00:06Z"
                )
                .unwrap(),
            1
        );
        assert!(store
            .get_pending_reply_notifications("agent-b", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn ambient_discussion_is_root_bound_transactional_and_delivery_free() {
        let store = SocialStore::open_in_temp().unwrap();
        let now = Utc::now();
        let max_thread_age_secs = 6 * 60 * 60;
        for id in ["agent-a", "agent-b"] {
            store
                .upsert_member(&Member {
                    member_id: id.into(),
                    kind: "agent".into(),
                    display_name: id.into(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: (now - ChronoDuration::minutes(1)).to_rfc3339(),
                })
                .unwrap();
        }
        let root = Post {
            post_id: "root".into(),
            author_id: "agent-a".into(),
            surface: "feed".into(),
            group_id: None,
            post_type: "question".into(),
            body: "Which recovery signal should stop rollout first?".into(),
            parent_id: None,
            created_at: (now - ChronoDuration::seconds(10)).to_rfc3339(),
        };
        assert!(store
            .insert_bounded_ambient_post(&root, 4, max_thread_age_secs, &now)
            .unwrap());
        assert!(!store
            .insert_bounded_ambient_post(
                &Post {
                    post_id: "competing-root".into(),
                    author_id: "agent-b".into(),
                    surface: "feed".into(),
                    group_id: None,
                    post_type: "thought".into(),
                    body: "This automatic topic must wait for the active root.".into(),
                    parent_id: None,
                    created_at: (now - ChronoDuration::seconds(9)).to_rfc3339(),
                },
                4,
                max_thread_age_secs,
                &now,
            )
            .unwrap());

        for index in 0..4 {
            assert!(store
                .insert_bounded_ambient_post(
                    &Post {
                        post_id: format!("reply-{index}"),
                        author_id: "agent-b".into(),
                        surface: "feed".into(),
                        group_id: None,
                        post_type: "reply".into(),
                        body: format!("Bounded contribution {index}"),
                        parent_id: Some("root".into()),
                        created_at: (now - ChronoDuration::seconds(8 - index)).to_rfc3339(),
                    },
                    4,
                    max_thread_age_secs,
                    &now,
                )
                .unwrap());
        }
        assert_eq!(store.direct_reply_count("root").unwrap(), 4);
        assert!(!store
            .insert_bounded_ambient_post(
                &Post {
                    post_id: "reply-over-cap".into(),
                    author_id: "agent-b".into(),
                    surface: "feed".into(),
                    group_id: None,
                    post_type: "reply".into(),
                    body: "This thread is already closed.".into(),
                    parent_id: Some("root".into()),
                    created_at: (now - ChronoDuration::seconds(3)).to_rfc3339(),
                },
                4,
                max_thread_age_secs,
                &now,
            )
            .unwrap());
        assert!(!store
            .insert_bounded_ambient_post(
                &Post {
                    post_id: "nested".into(),
                    author_id: "agent-a".into(),
                    surface: "feed".into(),
                    group_id: None,
                    post_type: "reply".into(),
                    body: "Nested automatic loops remain prohibited.".into(),
                    parent_id: Some("reply-0".into()),
                    created_at: (now - ChronoDuration::seconds(2)).to_rfc3339(),
                },
                4,
                max_thread_age_secs,
                &now,
            )
            .unwrap());
        assert!(store
            .open_public_discussion_root(4, max_thread_age_secs, &now)
            .unwrap()
            .is_none());
        assert!(store
            .insert_bounded_ambient_post(
                &Post {
                    post_id: "next-root".into(),
                    author_id: "agent-a".into(),
                    surface: "feed".into(),
                    group_id: None,
                    post_type: "question".into(),
                    body: "The closed discussion now permits one new topic.".into(),
                    parent_id: None,
                    created_at: (now - ChronoDuration::seconds(1)).to_rfc3339(),
                },
                4,
                max_thread_age_secs,
                &now,
            )
            .unwrap());
        assert!(store.get_pending_mentions(10).unwrap().is_empty());
        assert!(store
            .get_pending_reply_notifications("agent-a", 10)
            .unwrap()
            .is_empty());
        assert_eq!(store.get_feed_posts(10).unwrap().len(), 6);
    }

    #[test]
    fn ambient_discussion_ignores_stale_roots_and_rejects_stale_writes() {
        let store = SocialStore::open_in_temp().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-08-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let max_thread_age_secs = 6 * 60 * 60;
        for id in ["agent-a", "agent-b"] {
            store
                .upsert_member(&Member {
                    member_id: id.into(),
                    kind: "agent".into(),
                    display_name: id.into(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: "2026-08-26T00:00:00Z".into(),
                })
                .unwrap();
        }
        let stale_root = Post {
            post_id: "stale-root".into(),
            author_id: "agent-a".into(),
            surface: "feed".into(),
            group_id: None,
            post_type: "question".into(),
            body: "This historical root must no longer block discussion.".into(),
            parent_id: None,
            created_at: "2026-08-26T05:59:59Z".into(),
        };
        store.insert_post(&stale_root).unwrap();
        assert!(store
            .open_public_discussion_root(4, max_thread_age_secs, &now)
            .unwrap()
            .is_none());
        assert!(!store
            .insert_bounded_ambient_post(&stale_root, 4, max_thread_age_secs, &now)
            .unwrap());

        let fresh_root = Post {
            post_id: "fresh-root".into(),
            author_id: "agent-b".into(),
            created_at: "2026-08-26T06:00:01Z".into(),
            ..stale_root
        };
        assert!(store
            .insert_bounded_ambient_post(&fresh_root, 4, max_thread_age_secs, &now)
            .unwrap());
    }

    #[test]
    fn mention_parser_is_stable_deduplicated_and_does_not_parse_email_addresses() {
        assert_eq!(
            extract_mention_handles("@agent-a, again @agent-a; ask (@agent_b), not a@b.com"),
            vec!["agent-a", "agent_b"]
        );
    }

    #[test]
    fn explicit_delivery_requires_an_exact_visible_handle() {
        let store = SocialStore::open_in_temp().unwrap();
        for id in ["operator", "agent-a"] {
            store
                .upsert_member(&Member {
                    member_id: id.into(),
                    kind: if id == "operator" {
                        "operator"
                    } else {
                        "agent"
                    }
                    .into(),
                    display_name: id.into(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: "now".into(),
                })
                .unwrap();
        }
        let error = store
            .insert_post_with_delivery_targets(
                &Post {
                    post_id: "prefix-is-not-a-tag".into(),
                    author_id: "operator".into(),
                    surface: "feed".into(),
                    group_id: None,
                    post_type: "question".into(),
                    body: "@agent-alpha should not notify the shorter id".into(),
                    parent_id: None,
                    created_at: "2026-08-16T00:00:00Z".into(),
                },
                &["agent-a".to_string()],
            )
            .expect_err("a structured target cannot point at a prefix in visible text");
        assert!(error.to_string().contains("not visible"));
        assert!(store.get_feed_posts(10).unwrap().is_empty());
    }

    #[test]
    fn one_post_cannot_amplify_into_unbounded_mention_work() {
        let store = SocialStore::open_in_temp().unwrap();
        store
            .upsert_member(&Member {
                member_id: "operator".into(),
                kind: "operator".into(),
                display_name: "Operator".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        let mut body = String::new();
        for index in 0..12 {
            let id = format!("agent-{index}");
            store
                .upsert_member(&Member {
                    member_id: id.clone(),
                    kind: "agent".into(),
                    display_name: id.clone(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: "now".into(),
                })
                .unwrap();
            body.push_str(&format!("@{id} "));
        }
        let mentioned = store
            .insert_post_with_mentions(&Post {
                post_id: "bounded-mentions".into(),
                author_id: "operator".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "question".into(),
                body,
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .unwrap();
        assert_eq!(mentioned.len(), MAX_MENTIONS_PER_POST);
        assert_eq!(mentioned.first().map(String::as_str), Some("agent-0"));
        assert_eq!(mentioned.last().map(String::as_str), Some("agent-7"));
    }

    #[test]
    fn unknown_handles_do_not_consume_the_valid_delivery_cap() {
        let store = SocialStore::open_in_temp().unwrap();
        store
            .upsert_member(&Member {
                member_id: "operator".into(),
                kind: "operator".into(),
                display_name: "Operator".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        let mut body = (0..12)
            .map(|index| format!("@unknown-{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        for index in 0..MAX_MENTIONS_PER_POST {
            let id = format!("agent-{index}");
            store
                .upsert_member(&Member {
                    member_id: id.clone(),
                    kind: "agent".into(),
                    display_name: id.clone(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: "now".into(),
                })
                .unwrap();
            body.push_str(&format!(" @{id}"));
        }
        let delivered = store
            .insert_post_with_mentions(&Post {
                post_id: "unknowns-before-valid".into(),
                author_id: "operator".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "question".into(),
                body,
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .unwrap();
        assert_eq!(delivered.len(), MAX_MENTIONS_PER_POST);
        assert_eq!(delivered.last().map(String::as_str), Some("agent-7"));
    }

    #[tokio::test]
    async fn committed_public_mention_wakes_the_shared_worker_trigger() {
        let root = tempfile::tempdir().unwrap();
        let registry = SocialStoreRegistry::new(ArtifactV2Workspace::new(root.path()));
        let wake = registry.mention_notifier();
        let store = registry
            .store_for_scope(&ScopeRef::system_internal_unauthenticated(
                "anonymous",
                "default",
            ))
            .unwrap();
        store
            .upsert_member(&Member {
                member_id: "agent-a".into(),
                kind: "agent".into(),
                display_name: "Agent A".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        store
            .insert_post(&Post {
                post_id: "wake-post".into(),
                author_id: "operator".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "question".into(),
                body: "@agent-a please take a look".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_millis(50), wake.notified())
            .await
            .expect("mention notification permit survives until the worker awaits it");
        assert_eq!(
            wake.take_dirty_scopes(),
            HashSet::from([SocialScopeKey::new("anonymous", "default")])
        );
    }

    #[test]
    fn retention_is_bounded_and_preserves_the_newest_corpus() {
        let store = SocialStore::open_in_temp().unwrap();
        store
            .upsert_member(&Member {
                member_id: "agent-a".into(),
                kind: "agent".into(),
                display_name: "A".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        for (index, created_at) in [
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            "2026-08-15T00:00:00Z",
            "2026-08-16T00:00:00Z",
        ]
        .into_iter()
        .enumerate()
        {
            store
                .insert_post(&Post {
                    post_id: format!("post-{index}"),
                    author_id: "agent-a".into(),
                    surface: "feed".into(),
                    group_id: None,
                    post_type: "thought".into(),
                    body: format!("post {index}"),
                    parent_id: None,
                    created_at: created_at.into(),
                })
                .unwrap();
        }
        let report = store.prune_history("2026-08-01T00:00:00Z", 2, 2).unwrap();
        assert_eq!(report.posts_removed, 2);
        // The spend log retired with the engine; retention no longer touches it.
        assert_eq!(report.spend_rows_removed, 0);
        assert_eq!(
            store
                .get_feed_posts(10)
                .unwrap()
                .into_iter()
                .map(|post| post.post_id)
                .collect::<Vec<_>>(),
            vec!["post-3".to_string(), "post-2".to_string()]
        );
        // A store created after the retirement has no `spend_log` table at
        // all, which is exactly why retention must not query it.
        let spend_table: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'spend_log'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(spend_table, 0);
    }

    #[test]
    fn scoped_registry_never_shares_posts_between_workspaces() {
        let root = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(root.path());
        let registry = SocialStoreRegistry::new(workspace);
        let alpha = ScopeRef::system_internal_unauthenticated("alice", "alpha");
        let beta = ScopeRef::system_internal_unauthenticated("alice", "beta");
        let alpha_store = registry.store_for_scope(&alpha).unwrap();
        let beta_store = registry.store_for_scope(&beta).unwrap();
        alpha_store
            .upsert_member(&Member {
                member_id: "agent-a".into(),
                kind: "agent".into(),
                display_name: "A".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        alpha_store
            .insert_post(&Post {
                post_id: "post-a".into(),
                author_id: "agent-a".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "thought".into(),
                body: "alpha only".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .unwrap();
        assert_eq!(alpha_store.get_feed_posts(10).unwrap().len(), 1);
        assert!(beta_store.get_feed_posts(10).unwrap().is_empty());
        assert_ne!(
            registry.database_path_for_scope(&alpha),
            registry.database_path_for_scope(&beta)
        );
    }

    #[test]
    fn concurrent_first_open_is_single_flight_per_scope() {
        let root = tempfile::tempdir().unwrap();
        let registry = Arc::new(SocialStoreRegistry::new(ArtifactV2Workspace::new(
            root.path(),
        )));
        let scope_ref = ScopeRef::system_internal_unauthenticated("alice", "alpha");
        let barrier = Arc::new(std::sync::Barrier::new(12));
        let stores = std::thread::scope(|thread_scope| {
            let mut handles = Vec::new();
            for _ in 0..12 {
                let registry = Arc::clone(&registry);
                let scope_ref = scope_ref.clone();
                let barrier = Arc::clone(&barrier);
                handles.push(thread_scope.spawn(move || {
                    barrier.wait();
                    registry.store_for_scope(&scope_ref).unwrap()
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(stores.iter().all(|store| Arc::ptr_eq(&stores[0], store)));
        assert_eq!(
            stores[0]
                .get_all_members()
                .unwrap()
                .into_iter()
                .filter(|member| member.member_id == "operator")
                .count(),
            1
        );
    }

    #[test]
    fn legacy_global_store_moves_once_into_only_the_historical_default_scope() {
        let root = tempfile::tempdir().unwrap();
        let legacy = SocialStore::open(root.path()).unwrap();
        legacy
            .upsert_member(&Member {
                member_id: "operator".into(),
                kind: "operator".into(),
                display_name: "Operator".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        legacy
            .insert_post(&Post {
                post_id: "legacy-post".into(),
                author_id: "operator".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "thought".into(),
                body: "preserve this history".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .unwrap();
        drop(legacy);

        let registry = SocialStoreRegistry::new(ArtifactV2Workspace::new(root.path()));
        let other = registry
            .store_for_scope(&ScopeRef::system_internal_unauthenticated("alice", "alpha"))
            .unwrap();
        assert!(other.get_feed_posts(10).unwrap().is_empty());
        assert!(root.path().join("social.db").is_file());

        let default_store = registry
            .store_for_scope(&ScopeRef::system_internal_unauthenticated(
                "anonymous",
                "default",
            ))
            .unwrap();
        assert_eq!(
            default_store.get_feed_posts(10).unwrap()[0].post_id,
            "legacy-post"
        );
        assert!(!root.path().join("social.db").exists());
        assert!(registry
            .database_path_for_scope(&ScopeRef::system_internal_unauthenticated(
                "anonymous",
                "default"
            ))
            .is_file());

        let reused = SocialStoreRegistry::new(ArtifactV2Workspace::new(root.path()))
            .store_for_scope(&ScopeRef::system_internal_unauthenticated(
                "anonymous",
                "default",
            ))
            .unwrap();
        assert_eq!(reused.get_feed_posts(10).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_migration_never_writes_through_a_symlinked_scoped_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        drop(SocialStore::open(root.path()).unwrap());
        let outside = tempfile::tempdir().unwrap();
        let scoped_parent = root.path().join("scopes/anonymous/default");
        std::fs::create_dir_all(&scoped_parent).unwrap();
        symlink(outside.path(), scoped_parent.join("social")).unwrap();

        let registry = SocialStoreRegistry::new(ArtifactV2Workspace::new(root.path()));
        let error = match registry.store_for_scope(&ScopeRef::system_internal_unauthenticated(
            "anonymous",
            "default",
        )) {
            Ok(_) => panic!("legacy migration must reject a symlinked scoped directory"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("symbolic link"));
        assert!(root.path().join("social.db").is_file());
        assert!(!outside.path().join("social.db").exists());
    }

    #[cfg(unix)]
    #[test]
    fn scoped_store_rejects_a_symlinked_parent_component_before_sqlite_open() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("scopes")).unwrap();
        symlink(outside.path(), root.path().join("scopes/alice")).unwrap();

        let registry = SocialStoreRegistry::new(ArtifactV2Workspace::new(root.path()));
        let error = match registry.store_for_scope(&ScopeRef::system_internal_unauthenticated(
            "alice", "private",
        )) {
            Ok(_) => panic!("a parent scope symlink must not redirect the social database"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("symbolic link"));
        assert!(!outside.path().join("private/social/social.db").exists());
    }

    #[test]
    fn read_only_registry_lookup_does_not_materialize_missing_social_storage() {
        let root = tempfile::tempdir().unwrap();
        let registry = SocialStoreRegistry::new(ArtifactV2Workspace::new(root.path()));
        let scope = ScopeRef::system_internal_unauthenticated("alice", "unused");
        assert!(registry.existing_store_for_scope(&scope).unwrap().is_none());
        assert!(!registry.database_path_for_scope(&scope).exists());

        let store = registry.store_for_scope(&scope).unwrap();
        let operator = store
            .get_member("operator")
            .unwrap()
            .expect("materialization enrolls the scope operator");
        assert_eq!(operator.kind, "operator");
        assert_eq!(operator.display_name, "alice");

        // Reuse takes the once-per-scope path and preserves the original
        // identity rather than manufacturing another roster row.
        let reused = registry.store_for_scope(&scope).unwrap();
        assert!(Arc::ptr_eq(&store, &reused));
        assert_eq!(
            reused
                .get_all_members()
                .unwrap()
                .into_iter()
                .filter(|member| member.member_id == "operator")
                .count(),
            1
        );
    }

    #[test]
    fn roster_reconciliation_opts_out_absent_agents_without_erasing_history() {
        let store = SocialStore::open_in_temp().unwrap();
        for id in ["agent-a", "agent-b"] {
            store
                .upsert_member(&Member {
                    member_id: id.into(),
                    kind: "agent".into(),
                    display_name: id.into(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: "now".into(),
                })
                .unwrap();
        }
        let active = HashSet::from(["agent-a".to_string()]);
        assert_eq!(store.reconcile_agent_members(&active).unwrap(), 1);
        assert!(!store.get_member("agent-a").unwrap().unwrap().opted_out);
        assert!(store.get_member("agent-b").unwrap().unwrap().opted_out);
        let active_ids = store
            .get_active_members()
            .unwrap()
            .into_iter()
            .map(|member| member.member_id)
            .collect::<HashSet<_>>();
        assert!(active_ids.contains("agent-a"));
        assert!(!active_ids.contains("agent-b"));
    }

    #[test]
    fn foreign_keys_reject_posts_from_unenrolled_authors() {
        let store = SocialStore::open_in_temp().unwrap();
        let error = store
            .insert_post(&Post {
                post_id: "orphan".into(),
                author_id: "missing".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "thought".into(),
                body: "no author".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .expect_err("foreign key must reject orphan post");
        assert!(error.to_string().contains("FOREIGN KEY"));
    }

    #[test]
    fn opted_out_members_cannot_create_new_posts_or_reactions() {
        let store = SocialStore::open_in_temp().unwrap();
        for (member_id, kind, opted_out) in [
            ("operator", "operator", false),
            ("retired-agent", "agent", true),
        ] {
            store
                .upsert_member(&Member {
                    member_id: member_id.into(),
                    kind: kind.into(),
                    display_name: member_id.into(),
                    introversion: 0.5,
                    opted_out,
                    created_at: "now".into(),
                })
                .unwrap();
        }
        store
            .insert_post(&Post {
                post_id: "existing".into(),
                author_id: "operator".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "thought".into(),
                body: "Existing post".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .unwrap();

        let post_error = store
            .insert_post(&Post {
                post_id: "new-retired-post".into(),
                author_id: "retired-agent".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "thought".into(),
                body: "This must not land".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:01Z".into(),
            })
            .expect_err("an opted-out author must fail at the store boundary");
        assert!(post_error.to_string().contains("active roster member"));

        assert!(store
            .insert_reaction(&Reaction {
                post_id: "existing".into(),
                member_id: "retired-agent".into(),
                emoji: "👀".into(),
                created_at: "2026-08-16T00:00:02Z".into(),
            })
            .is_err());
        assert_eq!(store.get_feed_posts(10).unwrap().len(), 1);
        assert!(store.get_reactions_for_post("existing").unwrap().is_empty());
    }

    #[test]
    fn store_rejects_secret_shaped_content_even_when_a_writer_skips_validation() {
        let store = SocialStore::open_in_temp().unwrap();
        store
            .upsert_member(&Member {
                member_id: "agent-a".into(),
                kind: "agent".into(),
                display_name: "A".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        let error = store
            .insert_post(&Post {
                post_id: "unsafe".into(),
                author_id: "agent-a".into(),
                surface: "feed".into(),
                group_id: None,
                post_type: "thought".into(),
                body: "authorization: bearer definitely-not-safe".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .expect_err("the persistence boundary must reject unsafe content");
        assert!(error.to_string().contains("credential or secret"));
        assert!(store.get_feed_posts(10).unwrap().is_empty());
    }

    #[test]
    fn social_secret_boundary_reuses_high_confidence_token_detection() {
        for secret in [
            "ghp_abcdefghijklmnopqrstuvwxyz123456",
            "xoxb-1234567890-abcdefghijklmnop",
            "Bearer abcdefghijklmnopqrstuvwxyz",
            "postgres://user:password-value@example.test/db",
            "-----BEGIN EC PRIVATE KEY-----",
        ] {
            assert!(
                contains_secret_shaped_content(secret),
                "secret-shaped value escaped the social boundary: {secret}"
            );
        }
        assert!(!contains_secret_shaped_content(
            "We should discuss token budgets and API ergonomics."
        ));
    }

    #[test]
    fn composite_feed_cursor_does_not_skip_equal_timestamp_posts() {
        let store = SocialStore::open_in_temp().unwrap();
        store
            .upsert_member(&Member {
                member_id: "agent-a".into(),
                kind: "agent".into(),
                display_name: "A".into(),
                introversion: 0.5,
                opted_out: false,
                created_at: "now".into(),
            })
            .unwrap();
        let created_at = "2026-08-16T00:00:00Z";
        for post_id in ["post-a", "post-b", "post-c"] {
            store
                .insert_post(&Post {
                    post_id: post_id.into(),
                    author_id: "agent-a".into(),
                    surface: "feed".into(),
                    group_id: None,
                    post_type: "thought".into(),
                    body: post_id.into(),
                    parent_id: None,
                    created_at: created_at.into(),
                })
                .unwrap();
        }
        let first = store.get_feed_posts_before(2, None).unwrap();
        assert_eq!(
            first
                .iter()
                .map(|post| post.post_id.as_str())
                .collect::<Vec<_>>(),
            vec!["post-c", "post-b"]
        );
        let second = store
            .get_feed_posts_before(2, Some((&first[1].created_at, &first[1].post_id)))
            .unwrap();
        assert_eq!(
            second
                .iter()
                .map(|post| post.post_id.as_str())
                .collect::<Vec<_>>(),
            vec!["post-a"]
        );
    }

    #[test]
    fn private_group_author_and_reaction_visibility_are_enforced_in_store() {
        let store = SocialStore::open_in_temp().unwrap();
        for member_id in ["operator", "agent-a", "agent-b", "outsider"] {
            store
                .upsert_member(&Member {
                    member_id: member_id.into(),
                    kind: "agent".into(),
                    display_name: member_id.into(),
                    introversion: 0.5,
                    opted_out: false,
                    created_at: "now".into(),
                })
                .unwrap();
        }
        store
            .create_group(
                &Group {
                    group_id: "group-a".into(),
                    name: "Private".into(),
                    created_by: "operator".into(),
                    created_at: "now".into(),
                },
                &["operator", "agent-a", "agent-b"],
            )
            .unwrap();
        assert!(store
            .insert_post(&Post {
                post_id: "forbidden".into(),
                author_id: "outsider".into(),
                surface: "group".into(),
                group_id: Some("group-a".into()),
                post_type: "thought".into(),
                body: "no".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .is_err());
        store
            .insert_post(&Post {
                post_id: "private-post".into(),
                author_id: "agent-a".into(),
                surface: "group".into(),
                group_id: Some("group-a".into()),
                post_type: "thought".into(),
                body: "@agent-b private".into(),
                parent_id: None,
                created_at: "2026-08-16T00:00:00Z".into(),
            })
            .unwrap();
        store
            .insert_post(&Post {
                post_id: "private-reply".into(),
                author_id: "agent-b".into(),
                surface: "group".into(),
                group_id: Some("group-a".into()),
                post_type: "reply".into(),
                body: "private reply".into(),
                parent_id: Some("private-post".into()),
                created_at: "2026-08-16T00:00:01Z".into(),
            })
            .unwrap();
        assert!(store.get_pending_mentions(10).unwrap().is_empty());
        assert!(store
            .get_pending_reply_notifications("agent-a", 10)
            .unwrap()
            .is_empty());
        assert!(!store
            .is_post_visible_to("outsider", "private-post")
            .unwrap());
        assert!(store
            .is_post_visible_to("operator", "private-post")
            .unwrap());
        assert!(store
            .insert_reaction(&Reaction {
                post_id: "private-post".into(),
                member_id: "outsider".into(),
                emoji: "👍".into(),
                created_at: "2026-08-16T00:00:01Z".into(),
            })
            .is_err());
        store
            .insert_reaction(&Reaction {
                post_id: "private-post".into(),
                member_id: "operator".into(),
                emoji: "👍".into(),
                created_at: "2026-08-16T00:00:01Z".into(),
            })
            .unwrap();
    }
}
