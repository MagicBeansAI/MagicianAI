//! Lifecycle checks use the production pool and real SQLCipher databases.
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};

use super::registry_connections::{
    ConnectionIdentity, ConnectionPool, FileIdentity, RegistryConnectionLease,
};
use rusqlite::{Connection, OpenFlags};

#[test]
fn pooled_operation_measurements_exclude_checkout_validation_and_keep_real_query_counts() {
    let store = Store::new("measured");
    let pool = ConnectionPool::new(2);
    {
        let mut connection = store.checkout(&pool, true).unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO records(id, value) VALUES(1, ?1)",
                ["private-test-value"],
            )
            .unwrap();
        transaction.commit().unwrap();
    }
    let initial = pool.stats();
    assert_eq!(initial.completed_operations, 1);
    assert_eq!(initial.sql.statements, 3); // BEGIN, INSERT, COMMIT; no key/schema setup.
    assert_eq!(initial.sql.transactions_finished, 1);
    assert!(initial.checkout_ns > 0);
    assert!(initial.operation_ns > 0);
    for _ in 0..5 {
        let connection = store.checkout(&pool, true).unwrap();
        let count: u64 = connection
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
    let after = pool.stats();
    assert_eq!(after.opened, initial.opened);
    assert_eq!(after.reused - initial.reused, 5);
    assert_eq!(after.completed_operations - initial.completed_operations, 5);
    assert_eq!(after.sql.statements - initial.sql.statements, 5);
    assert_eq!(
        after.sql.transactions_finished,
        initial.sql.transactions_finished
    );
    assert_eq!(after.sql.unfinished_transactions, 0);
}

#[derive(Clone)]
struct Store {
    _directory: Arc<tempfile::TempDir>,
    path: PathBuf,
    owner: String,
    generation: Arc<AtomicU64>,
    full_validations: Arc<AtomicUsize>,
}

impl Store {
    fn new(owner: &str) -> Self {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        Self {
            path: directory.path().join("store.sqlite3"),
            _directory: directory,
            owner: owner.to_owned(),
            generation: Arc::new(AtomicU64::new(1)),
            full_validations: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn key(&self, generation: u64) -> String {
        format!("isolated-registry-pool-test:{}:{generation}", self.owner)
    }

    fn identity(&self) -> rusqlite::Result<ConnectionIdentity> {
        let file = std::fs::metadata(&self.path).ok().map(|metadata| {
            #[cfg(unix)]
            let (device, inode) = {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            };
            #[cfg(not(unix))]
            let (device, inode) = (0, 0);
            FileIdentity {
                device,
                inode,
                created: metadata.created().ok(),
            }
        });
        Ok(ConnectionIdentity {
            scope: ("fixture-principal".to_owned(), self.owner.clone()),
            key_generation: self.generation.load(Ordering::SeqCst).to_string(),
            file,
        })
    }

    fn open(&self, writable: bool) -> rusqlite::Result<Connection> {
        let flags = if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let connection = Connection::open_with_flags(&self.path, flags)?;
        let cipher: String =
            connection.pragma_query_value(None, "cipher_version", |row| row.get(0))?;
        assert!(
            !cipher.is_empty(),
            "this test must exercise SQLCipher, not plain SQLite"
        );
        connection.pragma_update(
            None,
            "key",
            self.key(self.generation.load(Ordering::SeqCst)),
        )?;
        connection.pragma_update(None, "cipher_compatibility", 4)?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        if writable {
            connection.pragma_update(None, "journal_mode", "WAL")?;
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS owner(name TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS records(id INTEGER PRIMARY KEY, value TEXT NOT NULL);",
            )?;
            connection.execute(
                "INSERT INTO owner(name) SELECT ?1 WHERE NOT EXISTS(SELECT 1 FROM owner)",
                [&self.owner],
            )?;
        }
        self.validate(&connection, true)?;
        Ok(connection)
    }

    fn validate(&self, connection: &Connection, full: bool) -> rusqlite::Result<()> {
        let owner: String = connection.query_row("SELECT name FROM owner", [], |row| row.get(0))?;
        if owner != self.owner {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if full {
            self.full_validations.fetch_add(1, Ordering::SeqCst);
            connection.prepare("SELECT id, value FROM records")?;
        }
        Ok(())
    }

    fn checkout(
        &self,
        pool: &Arc<ConnectionPool>,
        writable: bool,
    ) -> rusqlite::Result<RegistryConnectionLease> {
        pool.checkout(
            &self.path,
            writable,
            || self.identity(),
            || self.open(writable),
            |connection, full| self.validate(connection, full),
        )
    }

    fn count(&self, pool: &Arc<ConnectionPool>) -> i64 {
        self.checkout(pool, false)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap()
    }
}

#[test]
fn encrypted_warm_reads_and_writes_reuse_handles_and_keep_read_only_mode() {
    let pool = ConnectionPool::new(8);
    let store = Store::new("warm");
    drop(store.checkout(&pool, true).unwrap());
    assert_eq!(store.count(&pool), 0);
    let warmed = pool.stats();
    for id in 0..12 {
        store
            .checkout(&pool, true)
            .unwrap()
            .execute("INSERT INTO records VALUES(?1,'value')", [id])
            .unwrap();
        assert_eq!(store.count(&pool), id + 1);
    }
    let after = pool.stats();
    assert_eq!(
        after.opened, warmed.opened,
        "warm I/O must not repeat connection authentication"
    );
    assert_eq!(after.reused - warmed.reused, 24);
    assert_eq!(
        store.full_validations.load(Ordering::SeqCst),
        2,
        "unchanged schema must not be re-inspected per query"
    );
    let read = store.checkout(&pool, false).unwrap();
    assert!(read.is_readonly(rusqlite::DatabaseName::Main).unwrap());
    assert!(read
        .execute("INSERT INTO records VALUES(99,'forbidden')", [])
        .is_err());
}

#[test]
fn unfinished_transaction_is_discarded_and_rolled_back_before_next_user() {
    let pool = ConnectionPool::new(4);
    let store = Store::new("transaction");
    {
        let connection = store.checkout(&pool, true).unwrap();
        connection
            .execute_batch("BEGIN IMMEDIATE; INSERT INTO records VALUES(1,'uncommitted');")
            .unwrap();
    }
    assert_eq!(pool.stats().discarded, 1);
    assert_eq!(store.count(&pool), 0);
    assert_eq!(pool.stats().opened, 2);
}

#[test]
fn key_rotation_drains_cached_handles_and_opens_under_new_generation() {
    let pool = ConnectionPool::new(4);
    let store = Store::new("rotation");
    store
        .checkout(&pool, true)
        .unwrap()
        .execute("INSERT INTO records VALUES(1,'retained')", [])
        .unwrap();
    assert_eq!(store.count(&pool), 1);
    let before = pool.stats();
    pool.exclusive_key_rotation(&store.path, || {
        let connection = store.open(true).unwrap();
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
            .unwrap();
        connection
            .pragma_update(None, "rekey", store.key(2))
            .unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        store.generation.store(2, Ordering::SeqCst);
    });
    assert!(pool.stats().discarded >= before.idle as u64);
    store
        .checkout(&pool, true)
        .unwrap()
        .execute("INSERT INTO records VALUES(2,'new-key')", [])
        .unwrap();
    assert_eq!(store.count(&pool), 2);
    let encrypted_header = std::fs::read(&store.path).unwrap();
    assert!(!encrypted_header.starts_with(b"SQLite format 3"));
}

#[test]
fn coordinated_database_replacement_reopens_the_new_file() {
    let pool = ConnectionPool::new(4);
    let original = Store::new("replacement");
    original
        .checkout(&pool, true)
        .unwrap()
        .execute("INSERT INTO records VALUES(1,'old')", [])
        .unwrap();
    assert_eq!(original.count(&pool), 1);
    let replacement = Store::new("replacement");
    {
        let connection = replacement.open(true).unwrap();
        connection
            .execute_batch(
                "INSERT INTO records VALUES(1,'new'); INSERT INTO records VALUES(2,'new');
            PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;",
            )
            .unwrap();
    }
    // Maintenance drains WAL readers/writers before swapping database files.
    // Only isolated test databases are replaced; no runtime directory is used.
    pool.exclusive_database_maintenance(&original.path, || {
        let old = original.open(true).unwrap();
        old.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
            .unwrap();
        drop(old);
        std::fs::rename(&replacement.path, &original.path).unwrap();
    });
    assert_eq!(original.count(&pool), 2);
}

#[test]
fn stale_file_identity_never_returns_old_data_after_external_main_file_replacement() {
    let pool = ConnectionPool::new(4);
    let original = Store::new("uncoordinated-replacement");
    original
        .checkout(&pool, true)
        .unwrap()
        .execute("INSERT INTO records VALUES(1,'old')", [])
        .unwrap();
    assert_eq!(original.count(&pool), 1);
    let replacement = Store::new("uncoordinated-replacement");
    {
        let connection = replacement.open(true).unwrap();
        connection
            .execute_batch(
                "INSERT INTO records VALUES(2,'replacement');
            PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;",
            )
            .unwrap();
    }
    // Simulate an invalid external swap while the old WAL is live. The main
    // file has a different SQLCipher salt; mixing these generations must fail
    // closed rather than returning a still-readable cached old connection.
    std::fs::rename(&replacement.path, &original.path).unwrap();
    assert!(original.checkout(&pool, false).is_err());
}

#[test]
fn key_rotation_waits_for_active_leases_before_running_maintenance() {
    use std::sync::mpsc;
    use std::time::Duration;
    let pool = ConnectionPool::new(4);
    let store = Store::new("active-rotation");
    let active = store.checkout(&pool, true).unwrap();
    let (started_send, started_receive) = mpsc::channel();
    let (entered_send, entered_receive) = mpsc::channel();
    let rotating_pool = Arc::clone(&pool);
    let path = store.path.clone();
    let maintenance = std::thread::spawn(move || {
        started_send.send(()).unwrap();
        rotating_pool.exclusive_key_rotation(&path, || entered_send.send(()).unwrap());
    });
    started_receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(entered_receive
        .recv_timeout(Duration::from_millis(50))
        .is_err());
    active
        .execute("INSERT INTO records VALUES(1,'before-maintenance')", [])
        .unwrap();
    drop(active);
    entered_receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    maintenance.join().unwrap();
    assert_eq!(store.count(&pool), 1);
}

#[tokio::test]
async fn cancelled_async_caller_retains_lease_and_admission_until_blocking_work_finishes() {
    use tokio::sync::{oneshot, Mutex, Semaphore};
    let pool = ConnectionPool::new(4);
    let store = Store::new("cancelled-blocking-work");
    let slots = Arc::new(Semaphore::new(2));
    let scope = Arc::new(Mutex::new(()));
    let (entered_send, entered_receive) = oneshot::channel();
    let (released_send, released_receive) = std::sync::mpsc::channel();
    let (finished_send, finished_receive) = oneshot::channel();
    let (worker_pool, worker_store, worker_slots, worker_scope) = (
        Arc::clone(&pool),
        store.clone(),
        Arc::clone(&slots),
        Arc::clone(&scope),
    );
    let caller = tokio::spawn(async move {
        let ownership = super::registry_admission::write(worker_slots, worker_scope)
            .await
            .unwrap();
        tokio::task::spawn_blocking(move || {
            let _ownership = ownership;
            let connection = worker_store.checkout(&worker_pool, true).unwrap();
            connection
                .execute_batch("BEGIN IMMEDIATE; INSERT INTO records VALUES(1,'committed');")
                .unwrap();
            entered_send.send(()).unwrap();
            released_receive
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            connection.execute_batch("COMMIT").unwrap();
            drop(connection);
            drop(_ownership);
            finished_send.send(()).unwrap();
        })
        .await
        .unwrap();
    });
    entered_receive.await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert_eq!(slots.available_permits(), 1);
    assert!(Arc::clone(&scope).try_lock_owned().is_err());
    // The remaining worker is available to a different scoped database.
    let other_permit = super::registry_admission::read(Arc::clone(&slots))
        .await
        .unwrap();
    let other_pool = Arc::clone(&pool);
    tokio::task::spawn_blocking(move || {
        let _permit = other_permit;
        let other = Store::new("cancellation-independent");
        drop(other.checkout(&other_pool, true).unwrap());
        assert_eq!(other.count(&other_pool), 0);
    })
    .await
    .unwrap();
    released_send.send(()).unwrap();
    finished_receive.await.unwrap();
    assert_eq!(slots.available_permits(), 2);
    assert!(scope.try_lock_owned().is_ok());
    assert_eq!(store.count(&pool), 1);
}

#[test]
fn changed_schema_and_scope_are_revalidated_on_cached_connections() {
    let pool = ConnectionPool::new(4);
    let store = Store::new("schema");
    drop(store.checkout(&pool, true).unwrap());
    store
        .checkout(&pool, true)
        .unwrap()
        .execute_batch("CREATE TABLE extra(id INTEGER);")
        .unwrap();
    let before = store.full_validations.load(Ordering::SeqCst);
    drop(store.checkout(&pool, true).unwrap());
    assert_eq!(store.full_validations.load(Ordering::SeqCst), before + 1);
    store
        .checkout(&pool, true)
        .unwrap()
        .execute("UPDATE owner SET name='different-scope'", [])
        .unwrap();
    assert!(
        store.checkout(&pool, true).is_err(),
        "a cached handle is not authority to another scope"
    );
}

#[test]
fn bounded_idle_eviction_never_rejects_a_new_database() {
    let pool = ConnectionPool::new(2);
    let stores: Vec<_> = (0..6)
        .map(|id| Store::new(&format!("eviction-{id}")))
        .collect();
    for store in &stores {
        drop(store.checkout(&pool, true).unwrap());
    }
    assert_eq!(pool.stats().idle, 2);
    assert_eq!(pool.stats().discarded, 4);
    assert_eq!(stores[0].count(&pool), 0);
    assert_eq!(pool.stats().idle, 2);
}

#[test]
fn maintenance_verifies_checkpoints_and_reclaims_encrypted_pages_without_changing_records() {
    use super::registry_maintenance::{maintain, AppStoreMaintenanceAction as Action};
    let pool = ConnectionPool::new(4);
    let store = Store::new("maintenance");
    {
        let mut connection = store.checkout(&pool, true).unwrap();
        let transaction = connection.transaction().unwrap();
        for id in 0..120 {
            transaction
                .execute(
                    "INSERT INTO records VALUES(?1,?2)",
                    rusqlite::params![id, "retained-content".repeat(512)],
                )
                .unwrap();
        }
        // Isolated fixture creates unused pages. Production maintenance has
        // no logical deletion operation.
        transaction
            .execute("DELETE FROM records WHERE id >= 20", [])
            .unwrap();
        transaction.commit().unwrap();
    }
    assert_eq!(store.count(&pool), 20);
    store
        .checkout(&pool, true)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let size_before = std::fs::metadata(&store.path).unwrap().len();
    for action in [Action::Verify, Action::Optimize, Action::Reclaim] {
        pool.exclusive_database_maintenance(&store.path, || {
            let connection = store.open(action != Action::Verify).unwrap();
            let outcome = maintain(&connection, action).unwrap();
            assert!(!outcome.checkpoint_busy);
            assert!(outcome.page_count > 0);
            if action == Action::Reclaim {
                assert_eq!(outcome.free_pages, 0);
            }
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM records WHERE value = ?1",
                        ["retained-content".repeat(512)],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                20
            );
        });
        assert_eq!(store.count(&pool), 20);
    }
    assert!(std::fs::metadata(&store.path).unwrap().len() < size_before);
    assert!(!std::fs::read(&store.path)
        .unwrap()
        .starts_with(b"SQLite format 3"));
}

#[test]
fn database_maintenance_does_not_hold_up_an_independent_database() {
    let pool = ConnectionPool::new(4);
    let store = Store::new("maintained");
    let other = Store::new("working-during-maintenance");
    drop(store.checkout(&pool, true).unwrap());
    pool.exclusive_database_maintenance(&store.path, || {
        drop(other.checkout(&pool, true).unwrap());
        assert_eq!(other.count(&pool), 0);
    });
    assert_eq!(store.count(&pool), 0);
}

#[test]
fn independent_database_progresses_while_another_holds_a_write_transaction() {
    let pool = ConnectionPool::new(4);
    let busy = Store::new("busy");
    let other = Store::new("independent");
    let blocked = busy.checkout(&pool, true).unwrap();
    blocked
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO records VALUES(1,'pending');")
        .unwrap();
    other
        .checkout(&pool, true)
        .unwrap()
        .execute("INSERT INTO records VALUES(1,'completed')", [])
        .unwrap();
    assert_eq!(other.count(&pool), 1);
    assert!(
        !blocked.is_autocommit(),
        "the other database completed before this writer released"
    );
    drop(blocked);
    assert_eq!(busy.count(&pool), 0);
}

#[tokio::test]
async fn encrypted_mixed_app_load_progresses_with_competing_scope_maintenance() {
    use super::{registry_admission, registry_maintenance};
    use std::time::Instant;
    use tokio::sync::Semaphore;

    let pool = ConnectionPool::new(32);
    let slots = Arc::new(Semaphore::new(8));
    let stores = [Store::new("load-first"), Store::new("load-second")];
    for store in &stores {
        drop(store.checkout(&pool, true).unwrap());
        drop(store.checkout(&pool, false).unwrap());
    }
    let mut expected = [0; 2];
    for (phase, clients) in [2, 8, 24].into_iter().enumerate() {
        let before = pool.stats();
        let started = Instant::now();
        let mut jobs = Vec::new();
        for client in 0..clients {
            let store = stores[client % stores.len()].clone();
            expected[client % stores.len()] += 8;
            let pool = Arc::clone(&pool);
            let slots = Arc::clone(&slots);
            jobs.push(tokio::spawn(async move {
                let mut timings = Vec::new();
                for turn in 0..8 {
                    // Each client represents an App owning distinct row IDs;
                    // clients in the same scope still share SQLite's writer.
                    let id = (phase * 10000 + client * 100 + turn) as i64;
                    let value = format!("app-{client}-turn-{turn}");
                    let operation = Instant::now();
                    let admission = registry_admission::scoped_write(
                        Arc::clone(&slots),
                        registry_admission::database_write_lock(&store.path),
                        registry_admission::database_maintenance_lock(&store.path),
                    )
                    .await
                    .unwrap();
                    let write_store = store.clone();
                    let write_pool = Arc::clone(&pool);
                    let written = value.clone();
                    tokio::task::spawn_blocking(move || {
                        let _admission = admission;
                        let mut connection = write_store.checkout(&write_pool, true).unwrap();
                        let transaction = connection.transaction().unwrap();
                        transaction
                            .execute(
                                "INSERT INTO records VALUES(?1, ?2)",
                                rusqlite::params![id, written],
                            )
                            .unwrap();
                        transaction.commit().unwrap();
                    })
                    .await
                    .unwrap();
                    let admission = registry_admission::scoped_read(
                        Arc::clone(&slots),
                        registry_admission::database_maintenance_lock(&store.path),
                    )
                    .await
                    .unwrap();
                    let read_store = store.clone();
                    let read_pool = Arc::clone(&pool);
                    let actual: String = tokio::task::spawn_blocking(move || {
                        let _admission = admission;
                        read_store
                            .checkout(&read_pool, false)
                            .unwrap()
                            .query_row("SELECT value FROM records WHERE id = ?1", [id], |row| {
                                row.get(0)
                            })
                            .unwrap()
                    })
                    .await
                    .unwrap();
                    assert_eq!(actual, value);
                    timings.push(operation.elapsed().as_micros());
                }
                timings
            }));
        }
        let store = stores[0].clone();
        let maintenance_pool = Arc::clone(&pool);
        let maintenance = tokio::spawn(async move {
            let _admission = registry_admission::database_maintenance_lock(&store.path)
                .write_owned()
                .await;
            tokio::task::spawn_blocking(move || {
                maintenance_pool.exclusive_database_maintenance(&store.path, || {
                    let connection = store.open(true).unwrap();
                    let outcome = registry_maintenance::maintain(
                        &connection,
                        registry_maintenance::AppStoreMaintenanceAction::Optimize,
                    )
                    .unwrap();
                    assert!(!outcome.checkpoint_busy);
                });
            })
            .await
            .unwrap();
        });
        let mut timings = Vec::new();
        for job in jobs {
            timings.extend(job.await.unwrap());
        }
        maintenance.await.unwrap();
        timings.sort_unstable();
        let after = pool.stats();
        eprintln!("encrypted_app_load clients={clients} write_read_pairs={} elapsed_ms={} p50_us={} p95_us={} connection_opens={} connection_reuses={} statements={}", timings.len(), started.elapsed().as_millis(), timings[(timings.len()-1)/2], timings[(timings.len()*95).div_ceil(100)-1], after.opened-before.opened, after.reused-before.reused, after.sql.statements-before.sql.statements);
        assert_eq!(after.sql.unfinished_transactions, 0);
        assert!(after.reused > before.reused);
        assert_eq!(slots.available_permits(), 8);
        for (index, store) in stores.iter().enumerate() {
            assert_eq!(store.count(&pool), expected[index]);
            assert!(!std::fs::read(&store.path)
                .unwrap()
                .starts_with(b"SQLite format 3"));
        }
    }
}
