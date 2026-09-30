use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use duckdb::{AccessMode, Config, Connection};
use tracing::debug;

use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const BOOTSTRAP_DDL: &str = r#"
CREATE SEQUENCE IF NOT EXISTS event_id_seq;
CREATE TABLE IF NOT EXISTS events (
    id BIGINT DEFAULT nextval('event_id_seq'),
    timestamp TIMESTAMPTZ NOT NULL,
    event_type VARCHAR NOT NULL,
    source VARCHAR NOT NULL,
    payload JSON NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_type_ts ON events (event_type, timestamp);
"#;

/// Connection pool for the embedded DuckDB analytics database.
///
/// Maintains a single write connection behind a `Mutex` and hands out
/// independent read-only connections on demand.  All operations are
/// synchronous — callers are expected to run them on blocking threads.
pub struct DuckDbPool {
    db_path: PathBuf,
    bootstrap_sql: String,
    write_conn: Mutex<Connection>,
}

/// Open `db_path`, recovering from a stale/corrupt WAL by deleting it and
/// retrying once. Returns the underlying error if the main DB is itself
/// unreadable.
fn open_with_wal_recovery(db_path: &Path, wal_path: &Path) -> Result<Connection> {
    match Connection::open(db_path) {
        Ok(conn) => Ok(conn),
        Err(first_err) => {
            if wal_path.exists() {
                tracing::warn!(
                    target: "analytics",
                    path = %wal_path.display(),
                    error = %first_err,
                    "DuckDB open failed — deleting WAL and retrying"
                );
                let _ = std::fs::remove_file(wal_path);
                Connection::open(db_path).map_err(anyhow::Error::from)
            } else {
                Err(anyhow::Error::from(first_err))
            }
        },
    }
}

/// A lock conflict means another process/handle holds the DB — recoverable by
/// retrying later, NOT corruption. Anything else (incompatible storage version,
/// truncation, bad pages) is treated as unrecoverable so the caller can recreate.
fn is_lock_conflict(err: &anyhow::Error) -> bool {
    let message = err.to_string().to_lowercase();
    message.contains("lock") || message.contains("conflicting")
}

/// Sibling path for quarantining an unreadable DB, stamped so successive
/// quarantines don't collide.
fn quarantine_path(db_path: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!(
        "{}.incompatible-{}",
        db_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("analytics.duckdb"),
        stamp
    );
    let mut quarantine = db_path.to_path_buf();
    quarantine.set_file_name(name);
    quarantine
}

/// Repopulate the `events` table from the date-partitioned Parquet mirror the
/// event sink writes (`<analytics_root>/events/dt=*/*.parquet`). Parquet is
/// independent of DuckDB's on-disk storage format, so it survives version bumps
/// and makes quarantine-and-recreate lossless. Best-effort: a missing/empty
/// mirror is a no-op, logged for the operator.
fn rebuild_events_from_parquet(conn: &Connection, data_dir: &Path) {
    let events_dir = data_dir.join("events");
    if !events_dir.exists() {
        tracing::warn!(
            target: "analytics",
            "no events Parquet mirror found after quarantine; the recreated analytics DB starts empty (recover the quarantined file via the migration runbook if its history is needed)"
        );
        return;
    }
    let glob = events_dir.join("*").join("*.parquet");
    let glob_sql = glob.display().to_string().replace('\'', "''");
    let sql = format!(
        "INSERT INTO events (timestamp, event_type, source, payload) \
         SELECT to_timestamp(timestamp_ms / 1000.0), event_type, source, CAST(payload AS JSON) \
         FROM read_parquet('{glob_sql}');"
    );
    match conn.execute_batch(&sql) {
        Ok(()) => tracing::warn!(
            target: "analytics",
            mirror = %events_dir.display(),
            "rebuilt the analytics events table from the Parquet mirror after quarantine (no events lost)"
        ),
        Err(error) => tracing::warn!(
            target: "analytics",
            mirror = %events_dir.display(),
            error = %error,
            "could not rebuild events from the Parquet mirror (it may be empty); continuing with a fresh events table"
        ),
    }
}

impl DuckDbPool {
    pub fn open_scoped(
        workspace_layout: &ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> Result<Self> {
        let data_dir = workspace_layout.analytics_root(principal, workspace);
        let db_path = crate::magician_v2::database_owners::database_file_path(
            workspace_layout,
            principal,
            workspace,
            crate::magician_v2::database_owners::DatabaseOwner::AnalyticsDuckdb,
        );
        let schema_path = workspace_layout.analytics_db_template_schema_path();
        // Don't write into a read-only seed (container/deployment); the schema is
        // shipped there. open_internal reads it (or falls back to embedded DDL).
        if !workspace_layout.templates_are_read_only() {
            Self::ensure_template_seeded(&schema_path)?;
        }
        Self::open_internal(&data_dir, &db_path, Some(&schema_path))
    }

    /// Open (or create) the analytics database at `data_dir/analytics.duckdb`.
    ///
    /// Runs bootstrap DDL on first open to ensure the `events` table exists.
    pub fn open(data_dir: &Path) -> Result<Self> {
        let db_path = data_dir.join("analytics.duckdb");
        Self::open_internal(data_dir, &db_path, None)
    }

    fn open_internal(data_dir: &Path, db_path: &Path, schema_path: Option<&Path>) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating analytics data dir: {}", data_dir.display()))?;

        let wal_path = data_dir.join("analytics.duckdb.wal");
        debug!(target: "analytics", path = %db_path.display(), "opening analytics database");

        // Open with layered recovery:
        //   1. Plain open.
        //   2. On failure with a stale/corrupt WAL present, delete the WAL + retry.
        //   3. If it STILL fails and it is not a lock conflict, the main DB file
        //      itself is unreadable — almost always a DuckDB storage-version
        //      incompatibility after a dependency bump (e.g. a DB written by
        //      DuckDB v1.5.0 can't be opened by v1.5.2), or corruption from an
        //      ungraceful shutdown. Analytics is a *regenerable observability*
        //      store, so quarantine the file (renamed, never deleted — preserved
        //      for manual recovery/migration) and recreate a fresh DB rather than
        //      dropping every event and spamming open failures on every boot.
        // A lock conflict is NEVER treated as corruption: another process/handle
        // holds the DB, so we surface the error instead of destroying live data.
        let mut quarantined = false;
        let conn = match open_with_wal_recovery(db_path, &wal_path) {
            Ok(conn) => conn,
            Err(err) if is_lock_conflict(&err) => {
                return Err(err)
                    .with_context(|| format!("opening DuckDB at {}", db_path.display()));
            },
            Err(err) => {
                let quarantine = quarantine_path(db_path);
                // LOUD, never silent: the file is preserved (renamed, not deleted),
                // and the events table is rebuilt from the version-stable Parquet
                // mirror below. If there is no mirror (legacy DB written before the
                // mirror existed), recover it via the analytics migration runbook
                // (`make migrate-analytics`) — do NOT just accept the loss.
                tracing::error!(
                    target: "analytics",
                    db = %db_path.display(),
                    quarantine = %quarantine.display(),
                    error = %err,
                    "analytics DB is unreadable by this DuckDB build (incompatible storage version or corruption). Preserved the file (renamed, not deleted) and recreating a fresh DB; will rebuild events from the Parquet mirror. If events are missing afterwards, recover the quarantined file via the analytics migration runbook (make migrate-analytics)."
                );
                let _ = std::fs::rename(db_path, &quarantine);
                let _ = std::fs::remove_file(&wal_path);
                quarantined = true;
                Connection::open(db_path).with_context(|| {
                    format!(
                        "recreating DuckDB at {} after quarantine",
                        db_path.display()
                    )
                })?
            },
        };

        let bootstrap_sql = match schema_path {
            Some(path) if path.exists() => std::fs::read_to_string(path).with_context(|| {
                format!("reading analytics template schema: {}", path.display())
            })?,
            // No template file shipped (or a read-only seed without one): embedded DDL.
            _ => BOOTSTRAP_DDL.to_string(),
        };
        conn.execute_batch(&bootstrap_sql)
            .context("running analytics bootstrap schema")?;

        super::views::create_views(&conn).context("creating analytics convenience views")?;

        // Lossless self-heal: after quarantining an unreadable DB, repopulate the
        // events table from the version-stable Parquet mirror (Parquet is
        // independent of DuckDB's storage format, so it survives version bumps).
        // No-op when no mirror exists.
        if quarantined {
            rebuild_events_from_parquet(&conn, data_dir);
        }

        debug!(target: "analytics", "bootstrap DDL complete");

        Ok(Self {
            db_path: db_path.to_path_buf(),
            bootstrap_sql,
            write_conn: Mutex::new(conn),
        })
    }

    fn ensure_template_seeded(schema_path: &Path) -> Result<()> {
        let template_dir = schema_path
            .parent()
            .context("analytics template schema path missing parent directory")?;
        std::fs::create_dir_all(template_dir).with_context(|| {
            format!(
                "creating analytics template dir: {}",
                template_dir.display()
            )
        })?;
        if !schema_path.exists() {
            // This template is shared by EVERY scope — it takes no
            // principal/workspace — so a torn write here is not one scope's
            // problem, and `exists()` is true afterwards, so it never
            // self-heals. Publish it atomically.
            //
            // The `exists()` above is a TOCTOU and is **deliberately left
            // unlocked**, because here it is benign. `BOOTSTRAP_DDL` is a
            // compile-time constant, so two racers do not write *different*
            // content — they write identical bytes, each through its own unique
            // temp, each published by an atomic rename. Every interleaving ends
            // with the same file. A reader concurrent with either sees no file
            // and falls back to the embedded DDL, or sees the complete one.
            //
            // An earlier pass filed this as owed locking work. It is not: a
            // mutex would serialise two operations whose outcomes are already
            // identical. Revisit only if this ever writes something derived
            // rather than constant — then the check and the write do have to
            // become one operation.
            write_bytes_durably_sync(schema_path, BOOTSTRAP_DDL.as_bytes()).with_context(|| {
                format!(
                    "writing analytics template schema: {}",
                    schema_path.display()
                )
            })?;
        }
        Ok(())
    }

    /// Acquire the exclusive write connection.
    ///
    /// Returns a `MutexGuard` — hold it only as long as needed for the
    /// current batch insert, then drop it so other writers can proceed.
    pub fn write_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.write_conn
            .lock()
            .expect("analytics write connection mutex poisoned")
    }

    /// Replace the write connection with a fresh one.
    ///
    /// Call this after a fatal DuckDB error (e.g., disk full → invalidated state).
    /// The old connection is dropped and a new one is opened to the same database.
    pub fn reconnect_write(&self) -> Result<()> {
        let mut guard = self
            .write_conn
            .lock()
            .expect("analytics write mutex poisoned");
        let new_conn = Connection::open(&self.db_path)
            .with_context(|| format!("reconnecting DuckDB at {}", self.db_path.display()))?;
        new_conn
            .execute_batch(&self.bootstrap_sql)
            .context("re-running bootstrap schema after reconnect")?;
        super::views::create_views(&new_conn).context("re-creating views after reconnect")?;
        *guard = new_conn;
        debug!(target: "analytics", "DuckDB write connection reconnected");
        Ok(())
    }

    /// Open a new **read-only** connection to the same database file.
    ///
    /// Each caller gets its own connection, which is safe to use from a
    /// single thread without contention on the write mutex.
    pub fn read_connection(&self) -> Result<Connection> {
        let config = Config::default()
            .access_mode(AccessMode::ReadOnly)
            .context("configuring read-only access mode")?;

        Connection::open_with_flags(&self.db_path, config)
            .with_context(|| format!("opening read-only connection to {}", self.db_path.display()))
    }

    /// Path to the backing analytics DB file. Its parent directory is where the
    /// event sink writes the version-stable Parquet mirror (`events/dt=*/*.parquet`).
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Losslessly copy the live analytics catalog into a fresh DuckDB and swap
    /// it in only after exact schema/index/view and per-table row verification.
    pub fn compact(
        &self,
    ) -> Result<crate::magician_v2::storage_governance::DuckDbCompactionReport> {
        let mut connection = self
            .write_conn
            .lock()
            .expect("analytics write connection mutex poisoned");
        crate::magician_v2::storage_governance::compact_open_database(
            &mut connection,
            &self.db_path,
            |published| {
                published
                    .execute_batch(&self.bootstrap_sql)
                    .context("re-running analytics bootstrap after compaction")?;
                super::views::create_views(published)
                    .context("re-creating analytics views after compaction")?;
                Ok(())
            },
        )
    }
}

impl std::fmt::Debug for DuckDbPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckDbPool")
            .field("db_path", &self.db_path)
            .finish()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use tempfile::TempDir;

    #[test]
    fn open_creates_db_and_events_table() {
        let tmp = TempDir::new().unwrap();
        let pool = DuckDbPool::open(tmp.path()).unwrap();

        let conn = pool.read_connection().unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn open_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let _pool1 = DuckDbPool::open(tmp.path()).unwrap();
        drop(_pool1);
        // Second open on same dir should succeed (CREATE IF NOT EXISTS).
        let _pool2 = DuckDbPool::open(tmp.path()).unwrap();
    }

    #[test]
    fn open_fails_on_read_only_dir() {
        let tmp = TempDir::new().unwrap();
        let read_only_dir = tmp.path().join("locked");
        std::fs::create_dir(&read_only_dir).unwrap();

        // Make directory read-only so DuckDB can't create the .duckdb file.
        let mut perms = std::fs::metadata(&read_only_dir).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        {
            perms.set_readonly(true);
        }
        std::fs::set_permissions(&read_only_dir, perms.clone()).unwrap();

        let result = DuckDbPool::open(&read_only_dir);
        assert!(result.is_err(), "expected open to fail on read-only dir");

        // Restore permissions so TempDir cleanup succeeds.
        perms.set_readonly(false);
        std::fs::set_permissions(&read_only_dir, perms).unwrap();
    }

    #[test]
    fn write_and_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let pool = DuckDbPool::open(tmp.path()).unwrap();

        // Insert via write connection.
        {
            let conn = pool.write_connection();
            conn.execute_batch(
                "INSERT INTO events (timestamp, event_type, source, payload)
                 VALUES ('2026-03-17 12:00:00+00', 'test', 'unit_test', '{\"key\": \"value\"}')",
            )
            .unwrap();
        }

        // Read via separate read-only connection.
        let conn = pool.read_connection().unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let event_type: String = conn
            .query_row("SELECT event_type FROM events LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(event_type, "test");
    }

    #[test]
    fn convenience_views_exist() {
        let tmp = TempDir::new().unwrap();
        let pool = DuckDbPool::open(tmp.path()).unwrap();
        let conn = pool.read_connection().unwrap();

        // All typed views should be queryable (returning 0 rows on empty DB).
        for view in &[
            "logs",
            "bot_logs",
            "chat_messages",
            "chat_sessions",
            "task_executions",
            "task_steps",
            "artifacts",
        ] {
            let sql = format!("SELECT count(*) FROM {}", view);
            let count: i64 = conn.query_row(&sql, [], |row| row.get(0)).unwrap();
            assert_eq!(count, 0, "view {} should exist and return 0 rows", view);
        }
    }

    #[test]
    fn copy_compaction_preserves_events_indexes_and_convenience_views() {
        let tmp = TempDir::new().unwrap();
        let pool = DuckDbPool::open(tmp.path()).unwrap();
        {
            let connection = pool.write_connection();
            connection
                .execute_batch(
                    "INSERT INTO events (timestamp, event_type, source, payload) VALUES
                     ('2026-07-24 00:00:00+00', 'log', 'fixture', '{\"level\":\"info\",\"message\":\"kept\"}'),
                     ('2026-07-24 00:01:00+00', 'log', 'fixture', '{\"level\":\"debug\",\"message\":\"also kept\"}');",
                )
                .unwrap();
        }

        let report = pool.compact().unwrap();
        assert_eq!(report.row_count, 2);
        let connection = pool.read_connection().unwrap();
        let event_count: i64 = connection
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        let log_count: i64 = connection
            .query_row("SELECT count(*) FROM logs", [], |row| row.get(0))
            .unwrap();
        let index_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM duckdb_indexes() WHERE index_name = 'idx_events_type_ts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 2);
        assert_eq!(log_count, 2);
        assert_eq!(index_count, 1);
    }

    #[test]
    fn global_emit_is_noop_without_init() {
        // Before init(), emit() should silently discard events.
        super::super::emit(super::super::event_sink::AnalyticsEvent::log(
            "info",
            "test message",
            "test_target",
        ));
        // No panic, no error — just a no-op.
    }

    #[test]
    fn open_scoped_seeds_template_and_materializes_scoped_db() {
        let tmp = TempDir::new().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());

        let _pool = DuckDbPool::open_scoped(&workspace, "anonymous", "default").unwrap();

        assert!(workspace.analytics_db_template_schema_path().exists());
        assert!(workspace.analytics_db_path("anonymous", "default").exists());
    }

    #[test]
    fn template_schema_seed_publishes_atomically_and_leaves_no_staging_file() {
        let tmp = TempDir::new().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let schema_path = workspace.analytics_db_template_schema_path();

        DuckDbPool::ensure_template_seeded(&schema_path).unwrap();

        let template_dir = schema_path.parent().expect("template dir");
        let mut names: Vec<String> = std::fs::read_dir(template_dir)
            .expect("template dir listing")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["schema.sql".to_string()],
            "the durable write must publish exactly one file, with no staging sibling"
        );

        // "Parses" for a DDL template means DuckDB accepts it whole.
        let seeded = std::fs::read_to_string(&schema_path).expect("template body");
        assert_eq!(seeded, BOOTSTRAP_DDL);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&seeded)
            .expect("published template schema must execute");
    }
}
