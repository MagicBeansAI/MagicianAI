use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use duckdb::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
#[error("database recovery incomplete: {0}")]
pub struct RecoveryRequired(pub String);

fn recovery_marker(path: &Path) -> PathBuf {
    sibling_path(path, "maintenance-recovery.json")
}

/// Called under the owner's file lease, before opening any connection. Until
/// publication is verified and this marker is retired, the checkpointed old
/// generation is authoritative. New operations cannot enter during that window.
pub fn recover_interrupted_compaction(path: &Path) -> Result<()> {
    let marker = recovery_marker(path);
    if !marker.try_exists()? {
        return Ok(());
    }
    reject_unsafe_database_path(path)?;
    let metadata = std::fs::symlink_metadata(&marker)?;
    anyhow::ensure!(
        metadata.file_type().is_file() && metadata.len() <= 4096,
        "invalid database recovery marker"
    );
    let name: String = serde_json::from_slice(&std::fs::read(&marker)?)?;
    anyhow::ensure!(
        Path::new(&name).components().count() == 1
            && name.starts_with(&format!(
                "{}.storage-recovery-",
                path.file_name()
                    .context("database filename missing")?
                    .to_string_lossy()
            ))
            && name
                .rsplit('-')
                .next()
                .is_some_and(|id| id.parse::<ulid::Ulid>().is_ok()),
        "invalid database recovery backup"
    );
    let nonce = name
        .rsplit('-')
        .next()
        .context("recovery nonce missing")?
        .parse::<ulid::Ulid>()?;
    let backup = path.parent().context("database parent missing")?.join(name);
    anyhow::ensure!(
        std::fs::symlink_metadata(&backup)?.file_type().is_file(),
        "database recovery backup missing or unsafe"
    );
    let wal = duckdb_wal_path(path);
    if wal.try_exists()? {
        anyhow::ensure!(
            std::fs::symlink_metadata(&wal)?.file_type().is_file(),
            "unsafe database WAL"
        );
        std::fs::remove_file(&wal)?;
    }
    // Keep the backup until the marker is removed durably. A second crash
    // during recovery can repeat the same restoration safely.
    let restoring = sibling_path(path, &format!("storage-restore-{}", ulid::Ulid::new()));
    create_rollback_backup(&backup, &restoring)?;
    // This publishes an already-checkpointed multi-MiB database inode, not a
    // JSON/byte payload. The atomic-write helper would copy it into memory;
    // the unique sibling and directory fsync preserve the same durability.
    std::fs::rename(&restoring, path)?;
    // POSIX rename can be a no-op when both names already link the same inode
    // (a crash before publication). Retire that extra staging link too.
    remove_regular_file_if_present(&restoring);
    sync_parent(path)?;
    std::fs::remove_file(&marker)?;
    sync_parent(path)?;
    remove_regular_file_if_present(&backup);
    remove_regular_file_if_present(&sibling_path(path, &format!("storage-compact-{nonce}")));
    remove_regular_file_if_present(&sibling_path(path, &format!("storage-backup-{nonce}")));
    Ok(())
}

/// The caller has drained all readers and writers, and holds the writer lease.
pub fn compact_recoverable_database<F>(
    connection: &mut Connection,
    path: &Path,
    initialize: F,
) -> Result<DuckDbCompactionReport>
where
    F: Fn(&Connection) -> Result<()>,
{
    reject_unsafe_database_path(path)?;
    anyhow::ensure!(
        !recovery_marker(path).try_exists()?,
        "database recovery must complete before maintenance"
    );
    let size = std::fs::metadata(path)?.len();
    anyhow::ensure!(
        fs2::available_space(path.parent().context("database parent missing")?)?
            > size.saturating_mul(2).saturating_add(64 * 1024 * 1024),
        "insufficient space for verified database compaction"
    );
    connection.execute_batch("CHECKPOINT")?;
    sync_file(path)?;
    let nonce = ulid::Ulid::new();
    let backup = sibling_path(path, &format!("storage-recovery-{nonce}"));
    create_rollback_backup(path, &backup)?;
    sync_parent(path)?;
    let marker = recovery_marker(path);
    let bytes = serde_json::to_vec(
        &backup
            .file_name()
            .context("backup name missing")?
            .to_string_lossy(),
    )?;
    let operation = (|| {
        crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(&marker, &bytes)?;
        let report = compact_open_database_with_nonce(connection, path, &initialize, nonce)?;
        // Retiring the marker is the durable commit point. Cleanup errors also
        // take the recovery path before the caller can reopen admission.
        std::fs::remove_file(&marker)?;
        sync_parent(path)?;
        Ok::<_, anyhow::Error>(report)
    })();
    match operation {
        Ok(report) => {
            remove_regular_file_if_present(&backup);
            Ok(report)
        },
        Err(error) => {
            let recovery = (|| {
                let old = std::mem::replace(connection, Connection::open_in_memory()?);
                drop(old);
                // Marker publication/removal itself may have failed after rename.
                // Re-establish it while the immutable backup still exists.
                crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(&marker, &bytes)?;
                recover_interrupted_compaction(path)?;
                reopen_original(connection, path)?;
                Ok::<_, anyhow::Error>(())
            })();
            if let Err(recovery_error) = recovery {
                return Err(RecoveryRequired(format!("{error:#}; {recovery_error:#}")).into());
            }
            Err(error)
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuckDbCompactionReport {
    pub database: String,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub table_count: usize,
    pub row_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DatabaseFingerprint {
    table_rows: BTreeMap<String, u64>,
    table_sql: BTreeMap<String, String>,
    index_sql: BTreeMap<String, String>,
    view_sql: BTreeMap<String, String>,
    sequence_state: BTreeMap<String, String>,
}

impl DatabaseFingerprint {
    fn row_count(&self) -> u64 {
        self.table_rows.values().copied().sum()
    }
}

/// Losslessly rewrite an already-open DuckDB into a fresh file and atomically
/// replace the original. The owner must hold its process-local write mutex and
/// cross-process writer lease for the whole call.
///
/// `initialize` re-applies idempotent bootstrap DDL/views after the new file is
/// opened. The original file is retained as a sibling backup until both that
/// initialization and a second full fingerprint comparison succeed.
pub fn compact_open_database<F>(
    connection: &mut Connection,
    database_path: &Path,
    initialize: F,
) -> Result<DuckDbCompactionReport>
where
    F: Fn(&Connection) -> Result<()>,
{
    compact_open_database_with_nonce(connection, database_path, initialize, ulid::Ulid::new())
}

fn compact_open_database_with_nonce<F>(
    connection: &mut Connection,
    database_path: &Path,
    initialize: F,
    nonce: ulid::Ulid,
) -> Result<DuckDbCompactionReport>
where
    F: Fn(&Connection) -> Result<()>,
{
    reject_unsafe_database_path(database_path)?;
    connection
        .execute_batch("CHECKPOINT")
        .context("checkpointing DuckDB before copy compaction")?;

    let expected = fingerprint(connection).context("fingerprinting source DuckDB")?;
    let bytes_before = std::fs::metadata(database_path)
        .with_context(|| format!("reading metadata for {}", database_path.display()))?
        .len();
    let compact_path = sibling_path(database_path, &format!("storage-compact-{nonce}"));
    let backup_path = sibling_path(database_path, &format!("storage-backup-{nonce}"));
    let alias = "magician_storage_compact";
    let source_database: String = connection
        .query_row("SELECT current_database()", [], |row| row.get(0))
        .context("reading current DuckDB database name")?;
    let attach_path = escape_sql_literal(&compact_path.display().to_string());
    let copy_sql = format!(
        "ATTACH '{attach_path}' AS {alias}; COPY FROM DATABASE {} TO {alias}; CHECKPOINT {alias}; DETACH {alias};",
        quote_identifier(&source_database)
    );
    if let Err(error) = connection.execute_batch(&copy_sql) {
        remove_regular_file_if_present(&compact_path);
        return Err(error).context("copying DuckDB into compact destination");
    }
    sync_file(&compact_path)?;

    let compact_connection = Connection::open(&compact_path)
        .with_context(|| format!("opening compact DuckDB {}", compact_path.display()))?;
    // Verification opens a separate DuckDB instance: carry the caller's buffer
    // ceiling to it instead of silently using a machine-sized default.
    let memory_limit: String =
        connection.query_row("SELECT current_setting('memory_limit')", [], |row| {
            row.get(0)
        })?;
    compact_connection.execute_batch(&format!(
        "SET threads=1; SET memory_limit='{}'; SET max_temp_directory_size='2GiB';",
        escape_sql_literal(&memory_limit)
    ))?;
    let compact_fingerprint = fingerprint(&compact_connection)
        .context("fingerprinting compact DuckDB before publication")?;
    if compact_fingerprint != expected {
        drop(compact_connection);
        remove_regular_file_if_present(&compact_path);
        return Err(anyhow!(
            "DuckDB compaction verification failed before swap: schema, indexes, views, or row counts differ; {}",
            fingerprint_difference(&expected, &compact_fingerprint)
        ));
    }
    compact_connection
        .execute_batch("CHECKPOINT")
        .context("checkpointing verified compact DuckDB")?;
    drop(compact_connection);
    sync_file(&compact_path)?;

    // Replace the live handle with a harmless placeholder before renaming its
    // file. No owner method can observe it because the caller holds the store's
    // write mutex throughout this function.
    let placeholder = Connection::open_in_memory()
        .context("opening placeholder DuckDB during atomic compaction swap")?;
    let old_connection = std::mem::replace(connection, placeholder);
    drop(old_connection);
    remove_regular_file_if_present(&duckdb_wal_path(database_path));

    create_rollback_backup(database_path, &backup_path)?;
    sync_parent(database_path)?;
    if let Err(error) = std::fs::rename(&compact_path, database_path) {
        reopen_original(connection, database_path)?;
        remove_regular_file_if_present(&backup_path);
        return Err(error).context("publishing compact DuckDB");
    }
    sync_parent(database_path)?;

    let published = match Connection::open(database_path)
        .with_context(|| format!("opening published DuckDB {}", database_path.display()))
        .and_then(|published| {
            initialize(&published)?;
            let actual =
                fingerprint(&published).context("fingerprinting published compact DuckDB")?;
            if actual != expected {
                return Err(anyhow!(
                    "DuckDB compaction verification failed after publication"
                ));
            }
            published
                .execute_batch("CHECKPOINT")
                .context("checkpointing published compact DuckDB")?;
            Ok(published)
        }) {
        Ok(published) => published,
        Err(publish_error) => {
            std::fs::rename(&backup_path, database_path).with_context(|| {
                format!(
                    "restoring original DuckDB after failed compact publication: {publish_error}"
                )
            })?;
            sync_parent(database_path)?;
            reopen_original(connection, database_path)?;
            return Err(publish_error);
        },
    };
    *connection = published;
    let bytes_after = std::fs::metadata(database_path)
        .with_context(|| format!("reading compact metadata for {}", database_path.display()))?
        .len();
    remove_regular_file_if_present(&backup_path);
    sync_parent(database_path)?;

    Ok(DuckDbCompactionReport {
        database: database_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("duckdb")
            .to_string(),
        bytes_before,
        bytes_after,
        bytes_reclaimed: bytes_before.saturating_sub(bytes_after),
        table_count: expected.table_rows.len(),
        row_count: expected.row_count(),
    })
}

fn fingerprint_difference(expected: &DatabaseFingerprint, actual: &DatabaseFingerprint) -> String {
    let mut differences = Vec::new();
    if expected.table_rows != actual.table_rows {
        differences.push(format!(
            "table_rows expected={:?} actual={:?}",
            expected.table_rows, actual.table_rows
        ));
    }
    if expected.table_sql != actual.table_sql {
        differences.push(format!(
            "table_sql expected={:?} actual={:?}",
            expected.table_sql, actual.table_sql
        ));
    }
    if expected.index_sql != actual.index_sql {
        differences.push(format!(
            "index_sql expected={:?} actual={:?}",
            expected.index_sql, actual.index_sql
        ));
    }
    if expected.view_sql != actual.view_sql {
        differences.push(format!(
            "view_sql expected={:?} actual={:?}",
            expected.view_sql, actual.view_sql
        ));
    }
    if expected.sequence_state != actual.sequence_state {
        differences.push(format!(
            "sequence_state expected={:?} actual={:?}",
            expected.sequence_state, actual.sequence_state
        ));
    }
    differences.join("; ")
}

fn reopen_original(connection: &mut Connection, database_path: &Path) -> Result<()> {
    let original = Connection::open(database_path)
        .with_context(|| format!("reopening original DuckDB {}", database_path.display()))?;
    *connection = original;
    Ok(())
}

fn create_rollback_backup(database_path: &Path, backup_path: &Path) -> Result<()> {
    match std::fs::hard_link(database_path, backup_path) {
        Ok(()) => Ok(()),
        Err(link_error) => {
            std::fs::copy(database_path, backup_path).with_context(|| {
                format!(
                    "creating DuckDB rollback backup {} after hard-link failure: {link_error}",
                    backup_path.display()
                )
            })?;
            sync_file(backup_path)
        },
    }
}

fn fingerprint(connection: &Connection) -> Result<DatabaseFingerprint> {
    let mut table_sql = BTreeMap::new();
    let mut table_rows = BTreeMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT table_name, coalesce(sql, '') FROM duckdb_tables() WHERE NOT internal ORDER BY table_name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (name, sql) = row?;
            let count: i64 = connection.query_row(
                &format!("SELECT count(*) FROM {}", quote_identifier(&name)),
                [],
                |row| row.get(0),
            )?;
            table_rows.insert(name.clone(), u64::try_from(count).unwrap_or(0));
            table_sql.insert(name, normalize_sql(&sql));
        }
    }
    // `duckdb_indexes()` does not expose the `internal` flag in the DuckDB
    // version pinned by this workspace. It only returns created indexes, while
    // `duckdb_views()` also exposes system views and must be filtered.
    let index_sql = catalog_sql(connection, "duckdb_indexes()", "index_name", false)?;
    let view_sql = catalog_sql(connection, "duckdb_views()", "view_name", true)?;
    let sequence_state = sequence_state(connection)?;
    Ok(DatabaseFingerprint {
        table_rows,
        table_sql,
        index_sql,
        view_sql,
        sequence_state,
    })
}

fn sequence_state(connection: &Connection) -> Result<BTreeMap<String, String>> {
    // DuckDB's native database copy preserves the executable `START` value in
    // the sequence DDL but rewrites the catalog-only `last_value` from N to
    // N+1. Comparing `last_value` therefore rejects a semantically identical
    // copy. The DDL captures the next value; behavior is also regression-tested
    // with `nextval` after publication.
    let mut statement = connection.prepare(
        "SELECT sequence_name, coalesce(sql, '') FROM duckdb_sequences() ORDER BY sequence_name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| row.map(|(name, state)| (name, normalize_sql(&state))))
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()
        .map_err(anyhow::Error::from)
}

fn catalog_sql(
    connection: &Connection,
    table_function: &str,
    name_column: &str,
    filter_internal: bool,
) -> Result<BTreeMap<String, String>> {
    let predicate = if filter_internal {
        " WHERE NOT internal"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {name_column}, coalesce(sql, '') FROM {table_function}{predicate} ORDER BY {name_column}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| row.map(|(name, sql)| (name, normalize_sql(&sql))))
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()
        .map_err(anyhow::Error::from)
}

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn reject_unsafe_database_path(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting DuckDB path {}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(anyhow!(
            "DuckDB compaction source must be a regular file, not a symlink or special entry: {}",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("DuckDB path has no parent: {}", path.display()))?;
    let parent_metadata = std::fs::symlink_metadata(parent)?;
    if !parent_metadata.file_type().is_dir() {
        return Err(anyhow!(
            "DuckDB compaction parent must be a real directory: {}",
            parent.display()
        ));
    }
    Ok(())
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("database.duckdb");
    path.with_file_name(format!("{name}.{suffix}"))
}

fn duckdb_wal_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("database.duckdb");
    path.with_file_name(format!("{name}.wal"))
}

fn remove_regular_file_if_present(path: &Path) {
    if std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_file(path);
    }
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("opening {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("syncing {}", path.display()))
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    File::open(parent)
        .with_context(|| format!("opening {} for directory sync", parent.display()))?
        .sync_all()
        .with_context(|| format!("syncing directory {}", parent.display()))
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn recoverable_compaction_rolls_back_failed_publication_and_allows_retry() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.duckdb");
        let mut conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items(id INTEGER PRIMARY KEY); INSERT INTO items VALUES (7)",
        )
        .unwrap();
        let error = compact_recoverable_database(&mut conn, &path, |_| {
            anyhow::bail!("injected initialization failure")
        })
        .unwrap_err();
        assert!(!error.is::<RecoveryRequired>());
        assert!(!recovery_marker(&path).exists());
        conn.execute("INSERT INTO items VALUES (8)", []).unwrap();
        let report = compact_recoverable_database(&mut conn, &path, |_| Ok(())).unwrap();
        assert_eq!(report.row_count, 2);
        drop(conn);
        recover_interrupted_compaction(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("SELECT sum(id) FROM items", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            15
        );
    }

    #[test]
    fn restart_restores_checkpointed_generation_before_or_after_publication() {
        for published in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("fixture.duckdb");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE items(id INTEGER); INSERT INTO items VALUES (7); CHECKPOINT",
            )
            .unwrap();
            drop(conn);
            let backup = sibling_path(&path, &format!("storage-recovery-{}", ulid::Ulid::new()));
            create_rollback_backup(&path, &backup).unwrap();
            let marker = recovery_marker(&path);
            crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(
                &marker,
                &serde_json::to_vec(&backup.file_name().unwrap().to_string_lossy()).unwrap(),
            )
            .unwrap();
            if published {
                // Model the new inode after atomic publication; the backup
                // remains a hard link to the checkpointed old generation.
                std::fs::remove_file(&path).unwrap();
                let conn = Connection::open(&path).unwrap();
                conn.execute_batch(
                    "CREATE TABLE items(id INTEGER); INSERT INTO items VALUES (999); CHECKPOINT",
                )
                .unwrap();
            }
            recover_interrupted_compaction(&path).unwrap();
            recover_interrupted_compaction(&path).unwrap();
            let conn = Connection::open(&path).unwrap();
            assert_eq!(
                conn.query_row("SELECT id FROM items", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                7
            );
            assert!(!marker.exists());
            assert!(!backup.exists());
            assert!(!temp
                .path()
                .read_dir()
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().contains("storage-restore")));
        }
    }

    #[test]
    fn recovery_rejects_another_databases_backup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.duckdb");
        let conn = Connection::open(&path).unwrap();
        drop(conn);
        let name = format!("other.duckdb.storage-recovery-{}", ulid::Ulid::new());
        crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(
            &recovery_marker(&path),
            &serde_json::to_vec(&name).unwrap(),
        )
        .unwrap();
        assert!(recover_interrupted_compaction(&path)
            .unwrap_err()
            .to_string()
            .contains("invalid database recovery backup"));
    }

    #[test]
    fn copy_compaction_preserves_schema_indexes_views_and_rows() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let path = temporary.path().join("fixture.duckdb");
        let mut connection = Connection::open(&path).expect("open");
        connection
            .execute_batch(
                "CREATE SEQUENCE item_seq START 2000; \
                 CREATE TABLE items(id BIGINT DEFAULT nextval('item_seq') PRIMARY KEY, value VARCHAR); \
                 CREATE INDEX items_value ON items(value); \
                 INSERT INTO items SELECT i, repeat('x', 256) FROM range(0, 2000) t(i); \
                 INSERT INTO items(value) VALUES ('sequence-state'); \
                 CREATE VIEW item_view AS SELECT id, value FROM items; \
                 DELETE FROM items WHERE id < 1500; CHECKPOINT;",
            )
            .expect("seed churn");

        let report = compact_open_database(&mut connection, &path, |_| Ok(())).expect("compact");
        assert_eq!(report.table_count, 1);
        assert_eq!(report.row_count, 501);
        let rows: i64 = connection
            .query_row("SELECT count(*) FROM item_view", [], |row| row.get(0))
            .expect("query view");
        assert_eq!(rows, 501);
        connection
            .execute("INSERT INTO items(value) VALUES ('after-compact')", [])
            .expect("sequence remains usable");
        let generated: i64 = connection
            .query_row(
                "SELECT id FROM items WHERE value = 'after-compact'",
                [],
                |row| row.get(0),
            )
            .expect("generated id");
        assert_eq!(generated, 2001);
        assert!(!temporary
            .path()
            .read_dir()
            .expect("read tempdir")
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("storage-backup")));
    }

    #[cfg(unix)]
    #[test]
    fn copy_compaction_refuses_symlink_database() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("tempdir");
        let real = temporary.path().join("real.duckdb");
        let link = temporary.path().join("link.duckdb");
        let mut connection = Connection::open(&real).expect("open");
        connection
            .execute_batch("CREATE TABLE items(id BIGINT)")
            .expect("schema");
        symlink(&real, &link).expect("symlink");
        let error = compact_open_database(&mut connection, &link, |_| Ok(()))
            .expect_err("symlink must fail");
        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn failed_post_swap_initialization_restores_the_original_database() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let path = temporary.path().join("fixture.duckdb");
        let mut connection = Connection::open(&path).expect("open");
        connection
            .execute_batch("CREATE TABLE items(id BIGINT); INSERT INTO items VALUES (7)")
            .expect("seed");

        let error = compact_open_database(&mut connection, &path, |_| {
            Err(anyhow!("simulated migration failure"))
        })
        .expect_err("publication must roll back");
        assert!(error.to_string().contains("simulated migration failure"));
        let value: i64 = connection
            .query_row("SELECT id FROM items", [], |row| row.get(0))
            .expect("original remains open");
        assert_eq!(value, 7);
        assert!(!temporary
            .path()
            .read_dir()
            .expect("read tempdir")
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("storage-backup")));
    }
}
