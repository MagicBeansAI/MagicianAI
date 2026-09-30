use std::{
    sync::{mpsc, Mutex, MutexGuard, TryLockError},
    thread,
    time::{Duration, Instant},
};

use duckdb::{types::ValueRef, Connection, Error as DuckDbError};
use once_cell::sync::Lazy;
use tracing::warn;

static ANALYTICS_DUCKDB_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub const ANALYTICS_DUCKDB_MAX_RESULT_ROWS: usize = 10_000;
pub const ANALYTICS_DUCKDB_MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;

const ANALYTICS_DUCKDB_MEMORY_LIMIT: &str = "512MB";
const ANALYTICS_DUCKDB_MAX_TEMP_SIZE: &str = "1GB";

#[derive(Debug)]
pub enum AnalyticsDuckDbQueryError<E> {
    Query(E),
    TimedOut(Option<E>),
}

/// DuckDB's embedded library is process-local, and our analytics surfaces can
/// otherwise open many in-memory Parquet readers/writers concurrently. Keep the
/// lakehouse path serialized so UI dashboards cannot destabilize the runtime.
///
/// FIX #4 (Pulse de-serialization) decision — SERIALIZED LOCK KEPT, NOT relaxed
/// to a bounded read semaphore. A survey of every guard holder shows the two
/// entry points here are NOT a clean read/write split:
///   * `try_analytics_duckdb_guard_for` is held by pure in-memory `read_parquet`
///     reads (the Pulse query/batch handlers) BUT ALSO by heavy writers —
///     `parquet_maintenance` partition compaction and `llm_fact_compactor`
///     REWRITE the very Parquet files the read paths scan.
///   * `analytics_duckdb_guard` is held by the event/parquet sinks, memory-event
///     and trace compactors, repricing, and other maintenance that mutate shared
///     Parquet state and the pool's file-backed write connection.
/// Widening either entry point to N concurrent permits would let a compactor
/// rewrite a partition while a Pulse query reads it (torn/partial reads), so
/// true concurrency is unsafe without first reclassifying every call site into
/// read-only vs. exclusive — a large, higher-risk change out of scope here.
///
/// Instead we take the documented fallback: the guard is acquired ONCE across a
/// batched Pulse query. The `/analytics/llm_calls/query_batch` and
/// `/analytics/memory_events/query_batch` handlers grab the guard a single time,
/// build one in-memory DuckDB, and run all Pulse statements under it — collapsing
/// the frontend's 3 otherwise-serial single-query lock acquisitions into one.
/// New Pulse widgets MUST use those batch endpoints rather than firing parallel
/// single-query requests that would each contend for this lock.
pub fn analytics_duckdb_guard() -> MutexGuard<'static, ()> {
    match ANALYTICS_DUCKDB_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            warn!(
                target: "analytics::duckdb_safety",
                "analytics DuckDB lock was poisoned; continuing with recovered guard"
            );
            poisoned.into_inner()
        },
    }
}

pub fn try_analytics_duckdb_guard_for(timeout: Duration) -> Option<MutexGuard<'static, ()>> {
    let started_at = Instant::now();
    loop {
        match ANALYTICS_DUCKDB_LOCK.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => {
                warn!(
                    target: "analytics::duckdb_safety",
                    "analytics DuckDB lock was poisoned; continuing with recovered guard"
                );
                return Some(poisoned.into_inner());
            },
            Err(TryLockError::WouldBlock) if started_at.elapsed() >= timeout => return None,
            Err(TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(25));
            },
        }
    }
}

/// Apply every resource-safety setting independently and report failure to
/// callers that cannot safely continue with an unconstrained connection.
/// Independent statements avoid a version-incompatible setting preventing the
/// remaining compatible limits from being installed.
pub fn configure_analytics_connection_checked(
    conn: &Connection,
    context: &str,
) -> Result<(), DuckDbError> {
    let settings = [
        ("threads", "SET threads = 1".to_string()),
        (
            "memory_limit",
            format!("SET memory_limit = '{ANALYTICS_DUCKDB_MEMORY_LIMIT}'"),
        ),
        (
            "max_temp_directory_size",
            format!("SET max_temp_directory_size = '{ANALYTICS_DUCKDB_MAX_TEMP_SIZE}'"),
        ),
        (
            "preserve_insertion_order",
            "SET preserve_insertion_order = false".to_string(),
        ),
    ];

    let mut first_error = None;
    for (setting, statement) in settings {
        if let Err(error) = conn.execute_batch(&statement) {
            warn!(
                target: "analytics::duckdb_safety",
                context,
                setting,
                error = %error,
                "failed to configure analytics DuckDB connection"
            );
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Run a synchronous DuckDB operation with a connection-specific interrupt.
/// The caller remains blocked until the operation observes the interrupt and
/// exits, so any process-wide guard held by the caller cannot be released early.
pub fn run_analytics_query_with_interrupt_timeout<T, E, F>(
    conn: &Connection,
    timeout: Duration,
    operation: F,
) -> Result<T, AnalyticsDuckDbQueryError<E>>
where
    F: FnOnce() -> Result<T, E>,
{
    let interrupt = conn.interrupt_handle();
    let (done_tx, done_rx) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        let timed_out = done_rx.recv_timeout(timeout).is_err();
        if timed_out {
            interrupt.interrupt();
        }
        timed_out
    });

    let result = operation();
    let _ = done_tx.send(());
    let timed_out = watchdog.join().unwrap_or(false);

    if timed_out {
        Err(AnalyticsDuckDbQueryError::TimedOut(result.err()))
    } else {
        result.map_err(AnalyticsDuckDbQueryError::Query)
    }
}

/// Return an upper bound for the serialized size of borrowed variable-width
/// values. `None` means the value is fixed-width or nested and should be sized
/// after conversion. Text and blob are handled here to avoid cloning an
/// already-oversized cell into a result buffer.
pub fn duckdb_value_ref_output_bytes(value: ValueRef<'_>) -> Option<usize> {
    match value {
        ValueRef::Text(bytes) => Some(json_string_encoded_bytes(bytes)),
        ValueRef::Blob(bytes) => Some(
            bytes
                .len()
                .saturating_add(2)
                .checked_div(3)
                .unwrap_or(usize::MAX)
                .saturating_mul(4)
                .saturating_add(2),
        ),
        _ => None,
    }
}

pub fn json_string_encoded_bytes(bytes: &[u8]) -> usize {
    bytes.iter().fold(2usize, |size, byte| {
        size.saturating_add(match byte {
            b'"' | b'\\' | 0x08 | 0x09 | 0x0a | 0x0c | 0x0d => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn analytics_guard_serializes_process_wide_work() {
        let guard = analytics_duckdb_guard();
        let contender =
            thread::spawn(|| try_analytics_duckdb_guard_for(Duration::from_millis(50)).is_some());

        assert!(!contender.join().expect("guard contender"));
        drop(guard);
        assert!(try_analytics_duckdb_guard_for(Duration::from_secs(1)).is_some());
    }

    #[test]
    fn analytics_guard_timeout_does_not_overflow_instant() {
        assert!(try_analytics_duckdb_guard_for(Duration::MAX).is_some());
    }

    #[test]
    fn analytics_connection_uses_conservative_settings() {
        let conn = Connection::open_in_memory().expect("open DuckDB");
        configure_analytics_connection_checked(&conn, "duckdb_safety_test")
            .expect("configure DuckDB");

        let threads: i64 = conn
            .query_row("SELECT current_setting('threads')", [], |row| row.get(0))
            .expect("read threads setting");
        let memory_limit: String = conn
            .query_row("SELECT current_setting('memory_limit')", [], |row| {
                row.get(0)
            })
            .expect("read memory setting");
        let max_temp_size: String = conn
            .query_row(
                "SELECT current_setting('max_temp_directory_size')",
                [],
                |row| row.get(0),
            )
            .expect("read temp setting");

        assert_eq!(threads, 1);
        assert!(!memory_limit.is_empty());
        assert!(!max_temp_size.is_empty());
    }

    #[test]
    fn bundled_parquet_extension_is_available_without_runtime_install() {
        let conn = Connection::open_in_memory().expect("open DuckDB");
        configure_analytics_connection_checked(&conn, "duckdb_bundled_parquet_test")
            .expect("configure DuckDB");
        conn.execute_batch(
            "SET autoinstall_known_extensions = false; SET autoload_known_extensions = false;",
        )
        .expect("disable DuckDB extension installation and autoloading");

        let temp = tempfile::tempdir().expect("tempdir");
        let path_sql = temp
            .path()
            .join("bundled-parquet.parquet")
            .to_string_lossy()
            .replace('\'', "''");
        conn.execute_batch(&format!(
            "COPY (SELECT 7::BIGINT AS value) TO '{path_sql}' (FORMAT PARQUET, COMPRESSION 'zstd')"
        ))
        .expect("write Parquet without runtime extension loading");

        let value: i64 = conn
            .query_row(
                &format!("SELECT value FROM read_parquet('{path_sql}')"),
                [],
                |row| row.get(0),
            )
            .expect("read Parquet without runtime extension loading");
        assert_eq!(value, 7);
    }

    #[test]
    fn interrupt_timeout_waits_for_query_exit() {
        let conn = Connection::open_in_memory().expect("open DuckDB");
        configure_analytics_connection_checked(&conn, "duckdb_interrupt_test")
            .expect("configure DuckDB");

        let result =
            run_analytics_query_with_interrupt_timeout(&conn, Duration::from_millis(20), || {
                conn.execute_batch("SELECT count(*) FROM range(10000000) a, range(1000000) b")
            });

        assert!(matches!(
            result,
            Err(AnalyticsDuckDbQueryError::TimedOut(_))
        ));
    }
}
