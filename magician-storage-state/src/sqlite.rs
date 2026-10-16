use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use magician_storage::health::{HealthStatus, StorageHealth};
use magician_storage::StorageError;
use rusqlite::Connection;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Debug)]
pub struct SqlitePoolPolicy {
    pub max_connections: usize,
    pub busy_timeout: Duration,
    pub wal: bool,
}

impl Default for SqlitePoolPolicy {
    fn default() -> Self {
        Self {
            max_connections: 4,
            busy_timeout: Duration::from_secs(5),
            wal: true,
        }
    }
}

/// Bounded SQLite pool. Domain repositories must not return connections.
pub struct SqlitePool {
    path: PathBuf,
    policy: SqlitePoolPolicy,
    sem: Arc<Semaphore>,
}

pub struct PoolSlot {
    _permit: OwnedSemaphorePermit,
}

impl std::fmt::Debug for PoolSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PoolSlot")
    }
}

impl SqlitePool {
    pub fn open(path: impl AsRef<Path>, policy: SqlitePoolPolicy) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        let max = policy.max_connections.max(1);
        let pool = Self {
            path: path.clone(),
            policy,
            sem: Arc::new(Semaphore::new(max)),
        };
        let conn = pool.connect()?;
        drop(conn);
        Ok(pool)
    }

    pub fn health(&self) -> StorageHealth {
        StorageHealth {
            capability: "state_sqlite".into(),
            status: HealthStatus::Ok,
            safe_detail: format!(
                "wal={} max={}",
                self.policy.wal, self.policy.max_connections
            ),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Test helper: occupy a pool slot without exposing a connection.
    pub fn try_hold_slot(&self) -> Result<PoolSlot, StorageError> {
        let permit = self
            .sem
            .clone()
            .try_acquire_owned()
            .map_err(|_| StorageError::CapacityExceeded)?;
        Ok(PoolSlot { _permit: permit })
    }

    pub async fn with_conn<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StorageError::backend("sqlite pool closed"))?;
        let path = self.path.clone();
        let policy = self.policy.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut conn = connect_path(&path, &policy)?;
            f(&mut conn)
        })
        .await
        .map_err(|err| StorageError::backend(err.to_string()))?
    }

    pub fn schema_version(&self, store_id: &str) -> Result<u32, StorageError> {
        let conn = self.connect()?;
        ensure_ledger(&conn)?;
        conn.query_row(
            "SELECT version FROM schema_ledger WHERE store_id = ?1",
            [store_id],
            |row| row.get(0),
        )
        .or_else(|_| Ok(0))
        .map_err(sql_err)
    }

    pub async fn apply_migration(
        &self,
        store_id: &'static str,
        version: u32,
        up: &'static str,
    ) -> Result<(), StorageError> {
        let id = store_id.to_string();
        self.with_conn(move |conn| {
            ensure_ledger(conn)?;
            let current: u32 = conn
                .query_row(
                    "SELECT version FROM schema_ledger WHERE store_id = ?1",
                    [id.as_str()],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if current >= version {
                return Ok(());
            }
            conn.execute_batch(up).map_err(sql_err)?;
            conn.execute(
                "INSERT INTO schema_ledger(store_id, version) VALUES (?1, ?2)
                 ON CONFLICT(store_id) DO UPDATE SET version = excluded.version",
                rusqlite::params![id, version],
            )
            .map_err(sql_err)?;
            Ok(())
        })
        .await
    }

    pub async fn rollback_migration(
        &self,
        store_id: &'static str,
        version: u32,
        down: &'static str,
    ) -> Result<(), StorageError> {
        let id = store_id.to_string();
        self.with_conn(move |conn| {
            ensure_ledger(conn)?;
            conn.execute_batch(down).map_err(sql_err)?;
            conn.execute(
                "INSERT INTO schema_ledger(store_id, version) VALUES (?1, ?2)
                 ON CONFLICT(store_id) DO UPDATE SET version = excluded.version",
                rusqlite::params![id, version],
            )
            .map_err(sql_err)?;
            Ok(())
        })
        .await
    }

    fn connect(&self) -> Result<Connection, StorageError> {
        connect_path(&self.path, &self.policy)
    }
}

fn connect_path(path: &Path, policy: &SqlitePoolPolicy) -> Result<Connection, StorageError> {
    let conn = Connection::open(path).map_err(sql_err)?;
    if policy.wal {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(sql_err)?;
    }
    conn.busy_timeout(policy.busy_timeout).map_err(sql_err)?;
    Ok(conn)
}

pub(crate) fn ensure_ledger(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_ledger (
            store_id TEXT PRIMARY KEY,
            version INTEGER NOT NULL
        );",
    )
    .map_err(sql_err)
}

pub(crate) fn sql_err(err: rusqlite::Error) -> StorageError {
    StorageError::backend(err.to_string())
}
