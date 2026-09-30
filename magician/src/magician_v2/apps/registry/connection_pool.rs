//! Reuse authenticated connections; coordinate maintenance only for its database.
//!
//! Admission and authority belong to the registry caller. This pool bounds idle
//! resources, never the number of Apps, and never holds a database transaction
//! across asynchronous work. An owned read guard covers each synchronous lease;
//! key rotation takes the corresponding exclusive guard and drains idle handles.

use std::{
    collections::{HashMap, VecDeque},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Weak,
    },
};

use super::metrics::{nanos, ProfiledConnection, RegistrySqlStats};
use parking_lot::{lock_api::ArcRwLockReadGuard, Mutex, RawRwLock, RwLock};
use rusqlite::Connection;
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionIdentity {
    pub scope: (String, String),
    pub key_generation: String,
    pub file: Option<FileIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub created: Option<std::time::SystemTime>,
}

#[derive(Default)]
struct DatabaseState {
    identity: Option<ConnectionIdentity>,
    writable_ready: bool,
}

#[derive(Default)]
struct DatabaseGate {
    lock: Arc<RwLock<()>>,
    state: Mutex<DatabaseState>,
}

struct IdleConnection {
    database: Arc<DatabaseGate>,
    connection: ProfiledConnection,
    writable: bool,
    schema_cookie: i64,
}

#[derive(Default)]
struct Counters {
    opened: AtomicU64,
    reused: AtomicU64,
    discarded: AtomicU64,
    completed_operations: AtomicU64,
    checkout_ns: AtomicU64,
    operation_ns: AtomicU64,
    sql: Mutex<RegistrySqlStats>,
}

/// Counts include all callers sharing this pool, including registry clones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectionPoolStats {
    pub opened: u64,
    pub reused: u64,
    pub discarded: u64,
    pub idle: usize,
    pub completed_operations: u64,
    pub checkout_ns: u64,
    pub operation_ns: u64,
    pub sql: RegistrySqlStats,
}

pub(crate) struct ConnectionPool {
    max_idle: usize,
    key_epoch: Arc<RwLock<()>>,
    databases: Mutex<HashMap<PathBuf, Weak<DatabaseGate>>>,
    idle: Mutex<VecDeque<IdleConnection>>,
    counters: Counters,
}

impl ConnectionPool {
    pub(crate) fn new(max_idle: usize) -> Arc<Self> {
        Arc::new(Self {
            max_idle,
            key_epoch: Arc::new(RwLock::new(())),
            databases: Mutex::new(HashMap::new()),
            idle: Mutex::new(VecDeque::new()),
            counters: Counters::default(),
        })
    }

    pub(crate) fn stats(&self) -> ConnectionPoolStats {
        ConnectionPoolStats {
            opened: self.counters.opened.load(Ordering::Relaxed),
            reused: self.counters.reused.load(Ordering::Relaxed),
            discarded: self.counters.discarded.load(Ordering::Relaxed),
            idle: self.idle.lock().len(),
            completed_operations: self.counters.completed_operations.load(Ordering::Relaxed),
            checkout_ns: self.counters.checkout_ns.load(Ordering::Relaxed),
            operation_ns: self.counters.operation_ns.load(Ordering::Relaxed),
            sql: *self.counters.sql.lock(),
        }
    }

    fn database(&self, path: &Path) -> Arc<DatabaseGate> {
        let mut databases = self.databases.lock();
        if let Some(database) = databases.get(path).and_then(Weak::upgrade) {
            return database;
        }
        if databases.len() >= 256 {
            databases.retain(|_, value| value.strong_count() != 0);
        }
        let database = Arc::new(DatabaseGate::default());
        databases.insert(path.to_owned(), Arc::downgrade(&database));
        database
    }

    fn drain(&self, database: &Arc<DatabaseGate>) {
        // Close connections outside the global cache mutex: SQLite close may
        // checkpoint this database and must not stall an independent database.
        let retired = {
            let mut idle = self.idle.lock();
            let mut retained = VecDeque::new();
            let mut retired = Vec::new();
            while let Some(connection) = idle.pop_front() {
                if Arc::ptr_eq(&connection.database, database) {
                    retired.push(connection);
                } else {
                    retained.push_back(connection);
                }
            }
            *idle = retained;
            retired
        };
        self.counters
            .discarded
            .fetch_add(retired.len() as u64, Ordering::Relaxed);
        drop(retired);
    }

    /// Root-key rotation is rare explicit maintenance. Shared epoch guards do
    /// not serialize normal operations, but prevent a cold open from switching
    /// to a new root generation halfway through another checkout.
    pub(crate) fn exclusive_key_rotation<T>(
        &self,
        path: &Path,
        operation: impl FnOnce() -> T,
    ) -> T {
        let _key_epoch = self.key_epoch.write();
        let database = self.database(path);
        let _exclusive = database.lock.write();
        self.drain(&database);
        *database.state.lock() = DatabaseState::default();
        operation()
    }

    /// Owner-controlled checkpoint/replacement must first release every pooled
    /// handle, including readers. Never replace only a live SQLite main file:
    /// its WAL and shared-memory files belong to the same database generation.
    /// The callback must use an unpooled connection, not recursively checkout.
    pub(crate) fn exclusive_database_maintenance<T>(
        &self,
        path: &Path,
        operation: impl FnOnce() -> T,
    ) -> T {
        let _key_epoch = self.key_epoch.read();
        let database = self.database(path);
        let _exclusive = database.lock.write();
        self.drain(&database);
        *database.state.lock() = DatabaseState::default();
        operation()
    }

    /// `identity` is re-read after waiting for maintenance. `open` performs full
    /// schema/scope/encryption validation. `validate` checks a reused handle;
    /// its boolean argument requires full schema validation after DDL changes.
    pub(crate) fn checkout<E: From<rusqlite::Error>>(
        self: &Arc<Self>,
        path: &Path,
        writable: bool,
        identity: impl Fn() -> Result<ConnectionIdentity, E>,
        open: impl Fn() -> Result<Connection, E>,
        validate: impl Fn(&Connection, bool) -> Result<(), E>,
    ) -> Result<RegistryConnectionLease, E> {
        let checkout_started = Instant::now();
        let key_epoch = self.key_epoch.read_arc();
        let database = self.database(path);
        let mut guard = database.lock.read_arc();
        let requested_identity = identity()?;
        let current = {
            let state = database.state.lock();
            state.identity.as_ref() == Some(&requested_identity)
                && (!writable || state.writable_ready)
        };
        let mut initialized = None;
        if !current {
            drop(guard);
            let exclusive = database.lock.write_arc();
            // A preceding initializer may have finished while we waited.
            let requested_identity = identity()?;
            let current = {
                let state = database.state.lock();
                state.identity.as_ref() == Some(&requested_identity)
                    && (!writable || state.writable_ready)
            };
            if !current {
                self.drain(&database);
                *database.state.lock() = DatabaseState::default();
                let connection = ProfiledConnection::new(open()?)?;
                self.counters.opened.fetch_add(1, Ordering::Relaxed);
                let after = identity()?;
                // If another scope rotated the global keyring during open,
                // force the next checkout to initialize against its new epoch.
                if after.key_generation == requested_identity.key_generation {
                    *database.state.lock() = DatabaseState {
                        identity: Some(after),
                        writable_ready: writable,
                    };
                }
                initialized = Some(connection);
            }
            guard = parking_lot::lock_api::ArcRwLockWriteGuard::downgrade(exclusive);
        }

        let cached = if initialized.is_none() {
            let mut idle = self.idle.lock();
            idle.iter()
                .rposition(|item| {
                    Arc::ptr_eq(&item.database, &database) && item.writable == writable
                })
                .and_then(|index| idle.remove(index))
        } else {
            None
        };
        let (mut connection, schema_cookie, reused) = if let Some(cached) = cached {
            let validation: Result<i64, E> = (|| {
                let cookie = schema_cookie(&cached.connection)?;
                validate(&cached.connection, cookie != cached.schema_cookie)?;
                Ok(cookie)
            })();
            let cookie = match validation {
                Ok(cookie) => cookie,
                Err(error) => {
                    self.counters.discarded.fetch_add(1, Ordering::Relaxed);
                    return Err(error);
                },
            };
            self.counters.reused.fetch_add(1, Ordering::Relaxed);
            (cached.connection, cookie, true)
        } else {
            let connection = match initialized {
                Some(connection) => connection,
                None => {
                    let connection = ProfiledConnection::new(open()?)?;
                    self.counters.opened.fetch_add(1, Ordering::Relaxed);
                    connection
                },
            };
            let cookie = schema_cookie(&connection)?;
            (connection, cookie, false)
        };
        connection.reset_profile();
        Ok(RegistryConnectionLease {
            scope: requested_identity.scope,
            reused,
            checkout_ns: nanos(checkout_started.elapsed()),
            operation_started: Instant::now(),
            pool: Arc::clone(self),
            item: Some(IdleConnection {
                database,
                connection,
                writable,
                schema_cookie,
            }),
            _maintenance_guard: guard,
            _key_epoch: key_epoch,
        })
    }
}

fn schema_cookie(connection: &Connection) -> rusqlite::Result<i64> {
    connection.pragma_query_value(None, "schema_version", |row| row.get(0))
}

/// The handle never leaves its synchronous registry operation. Dropping an
/// async caller cannot release this lease while spawn_blocking still uses it.
pub struct RegistryConnectionLease {
    scope: (String, String),
    reused: bool,
    checkout_ns: u64,
    operation_started: Instant,
    pool: Arc<ConnectionPool>,
    item: Option<IdleConnection>,
    _maintenance_guard: ArcRwLockReadGuard<RawRwLock, ()>,
    _key_epoch: ArcRwLockReadGuard<RawRwLock, ()>,
}

impl std::fmt::Debug for RegistryConnectionLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryConnectionLease")
            .field("writable", &self.item.as_ref().map(|item| item.writable))
            .finish_non_exhaustive()
    }
}

impl Deref for RegistryConnectionLease {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self
            .item
            .as_ref()
            .expect("live connection lease")
            .connection
    }
}

impl DerefMut for RegistryConnectionLease {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self
            .item
            .as_mut()
            .expect("live connection lease")
            .connection
    }
}

impl Drop for RegistryConnectionLease {
    fn drop(&mut self) {
        let Some(item) = self.item.take() else {
            return;
        };
        let operation_ns = nanos(self.operation_started.elapsed());
        let sql = item.connection.sql_stats();
        self.pool
            .counters
            .completed_operations
            .fetch_add(1, Ordering::Relaxed);
        self.pool
            .counters
            .checkout_ns
            .fetch_add(self.checkout_ns, Ordering::Relaxed);
        self.pool
            .counters
            .operation_ns
            .fetch_add(operation_ns, Ordering::Relaxed);
        self.pool.counters.sql.lock().merge(sql);
        tracing::debug!(
            target: "magician::apps::registry",
            principal = %self.scope.0, workspace = %self.scope.1,
            writable = item.writable, reused = self.reused,
            checkout_ns = self.checkout_ns, operation_ns,
            statements = sql.statements, statement_ns = sql.statement_ns,
            longest_statement_ns = sql.longest_statement_ns,
            transactions_started = sql.transactions_started,
            transactions_finished = sql.transactions_finished,
            transaction_ns = sql.transaction_ns,
            longest_transaction_ns = sql.longest_transaction_ns,
            unfinished_transactions = sql.unfinished_transactions,
            "App registry connection operation finished"
        );
        // Do not carry a failed transaction, a panic or an I/O/corruption error
        // into the next caller. Connection close rolls back unfinished work.
        let code = unsafe { rusqlite::ffi::sqlite3_errcode(item.connection.handle()) } & 0xff;
        let healthy = item.connection.is_autocommit()
            && !std::thread::panicking()
            && !matches!(
                code,
                rusqlite::ffi::SQLITE_IOERR
                    | rusqlite::ffi::SQLITE_CORRUPT
                    | rusqlite::ffi::SQLITE_NOTADB
                    | rusqlite::ffi::SQLITE_MISUSE
            );
        if !healthy || self.pool.max_idle == 0 {
            self.pool.counters.discarded.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let evicted = {
            let mut idle = self.pool.idle.lock();
            idle.push_back(item);
            if idle.len() > self.pool.max_idle {
                idle.pop_front()
            } else {
                None
            }
        };
        if evicted.is_some() {
            self.pool.counters.discarded.fetch_add(1, Ordering::Relaxed);
        }
        drop(evicted);
    }
}
