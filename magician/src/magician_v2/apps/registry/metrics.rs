//! Numeric SQLite profiling attached to the actual connection. No SQL text,
//! bound values, keys or returned rows enter these measurements.
use rusqlite::{ffi, Connection};
use serde::Serialize;
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    ops::{Deref, DerefMut},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RegistrySqlStats {
    pub statements: u64,
    pub statement_ns: u64,
    pub longest_statement_ns: u64,
    pub transactions_started: u64,
    pub transactions_finished: u64,
    pub transaction_ns: u64,
    pub longest_transaction_ns: u64,
    pub unfinished_transactions: u64,
}

impl RegistrySqlStats {
    pub(super) fn merge(&mut self, other: Self) {
        self.statements = self.statements.saturating_add(other.statements);
        self.statement_ns = self.statement_ns.saturating_add(other.statement_ns);
        self.longest_statement_ns = self.longest_statement_ns.max(other.longest_statement_ns);
        self.transactions_started = self
            .transactions_started
            .saturating_add(other.transactions_started);
        self.transactions_finished = self
            .transactions_finished
            .saturating_add(other.transactions_finished);
        self.transaction_ns = self.transaction_ns.saturating_add(other.transaction_ns);
        self.longest_transaction_ns = self
            .longest_transaction_ns
            .max(other.longest_transaction_ns);
        self.unfinished_transactions = self
            .unfinished_transactions
            .saturating_add(other.unfinished_transactions);
    }
}

#[derive(Default)]
struct ProfileState {
    stats: RegistrySqlStats,
    transaction_started: Option<Instant>,
}

impl ProfileState {
    fn statement_finished(&mut self, now: Instant, duration: Duration, in_transaction: bool) {
        let elapsed = nanos(duration);
        self.stats.statements = self.stats.statements.saturating_add(1);
        self.stats.statement_ns = self.stats.statement_ns.saturating_add(elapsed);
        self.stats.longest_statement_ns = self.stats.longest_statement_ns.max(elapsed);
        match (self.transaction_started, in_transaction) {
            (None, true) => {
                self.stats.transactions_started = self.stats.transactions_started.saturating_add(1);
                self.transaction_started = Some(now.checked_sub(duration).unwrap_or(now));
            },
            (Some(started), false) => {
                let elapsed = nanos(now.saturating_duration_since(started));
                self.stats.transactions_finished =
                    self.stats.transactions_finished.saturating_add(1);
                self.stats.transaction_ns = self.stats.transaction_ns.saturating_add(elapsed);
                self.stats.longest_transaction_ns = self.stats.longest_transaction_ns.max(elapsed);
                self.transaction_started = None;
            },
            _ => {},
        }
    }
}

pub(super) fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

/// A queued future dropped by its caller records cancellation, not a timeout
/// invented by the registry. The measurement never changes admission policy.
pub(super) struct AdmissionMeasurement<'a> {
    started: Instant,
    principal: &'a str,
    workspace: &'a str,
    lane: &'static str,
    outcome: &'static str,
}

impl<'a> AdmissionMeasurement<'a> {
    pub(super) fn new(principal: &'a str, workspace: &'a str, lane: &'static str) -> Self {
        Self {
            started: Instant::now(),
            principal,
            workspace,
            lane,
            outcome: "cancelled",
        }
    }
    pub(super) fn finish(&mut self, admitted: bool) {
        self.outcome = if admitted { "admitted" } else { "closed" };
    }
}

impl Drop for AdmissionMeasurement<'_> {
    fn drop(&mut self) {
        tracing::debug!(target: "magician::apps::registry",
            principal = self.principal, workspace = self.workspace,
            lane = self.lane, outcome = self.outcome,
            wait_ns = nanos(self.started.elapsed()), "App registry admission finished");
    }
}

/// The boxed callback state stays at the same address when a pooled handle
/// moves between workers. Connection is !Sync; callbacks and snapshots execute
/// synchronously on its current owner thread. No thread-local attribution is
/// used, so nested operations on different databases cannot mix their counts.
pub(super) struct ProfiledConnection {
    connection: Connection,
    profile: Box<UnsafeCell<ProfileState>>,
}

impl ProfiledConnection {
    pub(super) fn new(connection: Connection) -> rusqlite::Result<Self> {
        let profile = Box::new(UnsafeCell::new(ProfileState::default()));
        // SAFETY: SQLite retains only the stable boxed context pointer. This
        // owner unregisters the callback before dropping either the connection
        // or its context. PROFILE supplies a live statement and i64 duration.
        let code = unsafe {
            ffi::sqlite3_trace_v2(
                connection.handle(),
                ffi::SQLITE_TRACE_PROFILE as u32,
                Some(profile_callback),
                profile.get().cast::<c_void>(),
            )
        };
        if code != ffi::SQLITE_OK {
            unsafe {
                ffi::sqlite3_trace_v2(connection.handle(), 0, None, std::ptr::null_mut());
            }
            return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None));
        }
        Ok(Self {
            connection,
            profile,
        })
    }

    /// Called after checkout validation and before the caller's operation.
    pub(super) fn reset_profile(&mut self) {
        // Exclusive access and no SQLite call: no callback can be active.
        *self.profile.get_mut() = ProfileState::default();
    }

    pub(super) fn sql_stats(&self) -> RegistrySqlStats {
        // SAFETY: no SQLite call can execute concurrently on this !Sync owner.
        let state = unsafe { &*self.profile.get() };
        let mut stats = state.stats;
        stats.unfinished_transactions = u64::from(state.transaction_started.is_some());
        stats
    }
}

impl Deref for ProfiledConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.connection
    }
}
impl DerefMut for ProfiledConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}
impl Drop for ProfiledConnection {
    fn drop(&mut self) {
        // SAFETY: both handle and boxed context are still alive. After this
        // synchronous call, closing SQLite cannot access the context again.
        unsafe {
            ffi::sqlite3_trace_v2(self.connection.handle(), 0, None, std::ptr::null_mut());
        }
    }
}

unsafe extern "C" fn profile_callback(
    event: u32,
    context: *mut c_void,
    statement: *mut c_void,
    elapsed: *mut c_void,
) -> i32 {
    if event != ffi::SQLITE_TRACE_PROFILE as u32
        || context.is_null()
        || statement.is_null()
        || elapsed.is_null()
    {
        return 0;
    }
    // Never unwind across SQLite's C ABI, including during a Rust panic that
    // drops an unfinished statement. This callback never formats SQL or errors.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let duration = unsafe { *elapsed.cast::<i64>() }.max(0) as u64;
        let database = unsafe { ffi::sqlite3_db_handle(statement.cast::<ffi::sqlite3_stmt>()) };
        let in_transaction = unsafe { ffi::sqlite3_get_autocommit(database) } == 0;
        let state = unsafe { &mut *context.cast::<ProfileState>() };
        state.statement_finished(
            Instant::now(),
            Duration::from_nanos(duration),
            in_transaction,
        );
    }));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_sqlite_statements_and_transactions_are_counted_without_content() {
        let mut connection =
            ProfiledConnection::new(Connection::open_in_memory().unwrap()).unwrap();
        connection
            .execute_batch("CREATE TABLE records(value TEXT)")
            .unwrap();
        connection.reset_profile();
        {
            let transaction = connection.transaction().unwrap();
            transaction
                .execute(
                    "INSERT INTO records VALUES (?1)",
                    ["private-value-never-exported"],
                )
                .unwrap();
            transaction
                .execute("UPDATE records SET value = ?1", ["another-private-value"])
                .unwrap();
            transaction.commit().unwrap();
        }
        let stats = connection.sql_stats();
        assert_eq!(stats.statements, 4);
        assert_eq!(
            (stats.transactions_started, stats.transactions_finished),
            (1, 1)
        );
        assert_eq!(stats.unfinished_transactions, 0);
        let encoded = serde_json::to_value(stats).unwrap();
        assert!(encoded
            .as_object()
            .unwrap()
            .values()
            .all(|value| value.is_u64()));
        assert!(stats.transaction_ns > 0);
    }

    #[test]
    fn interleaved_connections_and_rollback_keep_separate_profiles() {
        let first = ProfiledConnection::new(Connection::open_in_memory().unwrap()).unwrap();
        let second = ProfiledConnection::new(Connection::open_in_memory().unwrap()).unwrap();
        first.execute_batch("BEGIN; SELECT 1").unwrap();
        second
            .execute_batch("SELECT 2; SELECT 3; SELECT 4")
            .unwrap();
        assert_eq!(first.sql_stats().statements, 2);
        assert_eq!(second.sql_stats().statements, 3);
        assert_eq!(first.sql_stats().unfinished_transactions, 1);
        assert_eq!(second.sql_stats().transactions_started, 0);
        first.execute_batch("ROLLBACK").unwrap();
        assert_eq!(first.sql_stats().transactions_finished, 1);
        assert_eq!(first.sql_stats().unfinished_transactions, 0);
    }

    #[test]
    fn profile_survives_worker_transfer_and_counts_real_encrypted_io() {
        let directory = tempfile::tempdir().unwrap();
        let raw = Connection::open(directory.path().join("encrypted.sqlite3")).unwrap();
        let cipher: String = raw
            .pragma_query_value(None, "cipher_version", |row| row.get(0))
            .unwrap();
        assert!(!cipher.is_empty());
        raw.pragma_update(None, "key", "isolated-metrics-test-key")
            .unwrap();
        let mut connection = ProfiledConnection::new(raw).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE records(id INTEGER); BEGIN; INSERT INTO records VALUES(7); COMMIT",
            )
            .unwrap();
        let mut connection = std::thread::spawn(move || {
            connection.reset_profile();
            let value: u64 = connection
                .query_row("SELECT id FROM records", [], |row| row.get(0))
                .unwrap();
            assert_eq!(value, 7);
            assert_eq!(connection.sql_stats().statements, 1);
            connection
        })
        .join()
        .unwrap();
        connection.reset_profile();
        assert_eq!(connection.sql_stats(), RegistrySqlStats::default());
        let mut aggregate = RegistrySqlStats::default();
        connection.execute_batch("SELECT 1").unwrap();
        aggregate.merge(connection.sql_stats());
        aggregate.merge(connection.sql_stats());
        assert_eq!(aggregate.statements, 2);
    }
}
