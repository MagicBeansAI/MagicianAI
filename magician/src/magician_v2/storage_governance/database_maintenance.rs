//! Bounded admission and resource policy for long-lived embedded databases.
use std::{
    ops::Deref,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, ensure, Result};
use duckdb::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseMaintenanceConfig {
    pub enabled: bool,
    pub startup_delay_seconds: u64,
    pub check_interval_seconds: u64,
    pub cooldown_seconds: u64,
    pub max_deferral_seconds: u64,
    pub min_database_mib: u64,
    pub fragmented_row_groups: u64,
    pub channel_memory_mib: u64,
    pub feed_memory_mib: u64,
    pub maintenance_memory_mib: u64,
}

impl Default for DatabaseMaintenanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            startup_delay_seconds: 15,
            check_interval_seconds: 1800,
            cooldown_seconds: 86400,
            max_deferral_seconds: 21600,
            min_database_mib: 64,
            fragmented_row_groups: 128,
            channel_memory_mib: 1024,
            feed_memory_mib: 256,
            maintenance_memory_mib: 2048,
        }
    }
}

impl DatabaseMaintenanceConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=86400).contains(&self.startup_delay_seconds),
            "invalid database maintenance startup delay"
        );
        ensure!(
            (60..=86400).contains(&self.check_interval_seconds),
            "invalid database maintenance check interval"
        );
        ensure!(
            (60..=604800).contains(&self.cooldown_seconds),
            "invalid database maintenance cooldown"
        );
        ensure!(
            (60..=86400).contains(&self.max_deferral_seconds),
            "invalid database maintenance deferral"
        );
        ensure!(
            (1..=4096).contains(&self.min_database_mib),
            "invalid database maintenance minimum size"
        );
        ensure!(
            (8..=10000).contains(&self.fragmented_row_groups),
            "invalid database fragmentation threshold"
        );
        for size in [
            self.channel_memory_mib,
            self.feed_memory_mib,
            self.maintenance_memory_mib,
        ] {
            ensure!(
                (64..=4096).contains(&size),
                "database memory budget must be 64..4096 MiB"
            );
        }
        ensure!(
            self.maintenance_memory_mib >= self.channel_memory_mib.max(self.feed_memory_mib),
            "maintenance budget must cover normal database budgets"
        );
        Ok(())
    }
}

pub fn configure_connection(conn: &Connection, memory_mib: u64) -> Result<()> {
    ensure!(
        (64..=4096).contains(&memory_mib),
        "invalid database memory budget"
    );
    conn.execute_batch(&format!(
        "SET threads=1; SET memory_limit='{memory_mib}MiB'; SET max_temp_directory_size='2GiB';"
    ))?;
    Ok(())
}

#[derive(Debug, Default)]
struct Admission {
    active: usize,
    waiting: usize,
    maintenance: bool,
    recovery_required: bool,
}

#[derive(Debug, Default)]
pub struct DatabaseGate {
    state: Mutex<Admission>,
    changed: Condvar,
}

impl DatabaseGate {
    /// An uncertain publication must fail closed until startup recovery runs.
    pub fn require_recovery(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.recovery_required = true;
        self.changed.notify_all();
    }

    pub fn enter(self: &Arc<Self>) -> Result<DatabasePermit> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("database admission lock poisoned"))?;
        ensure!(
            state.waiting < 64,
            "database maintenance queue is full; retry shortly"
        );
        state.waiting += 1;
        while state.maintenance && !state.recovery_required {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.waiting -= 1;
                return Err(anyhow!(
                    "database maintenance is in progress; retry shortly"
                ));
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| anyhow!("database admission lock poisoned"))?
                .0;
        }
        state.waiting -= 1;
        ensure!(
            !state.recovery_required,
            "database requires recovery; restart the service"
        );
        state.active += 1;
        Ok(DatabasePermit(Arc::clone(self)))
    }

    pub fn maintain(self: &Arc<Self>, wait: Duration) -> Result<MaintenancePermit> {
        let deadline = Instant::now() + wait;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("database admission lock poisoned"))?;
        ensure!(!state.maintenance, "database maintenance already running");
        ensure!(
            !state.recovery_required,
            "database requires recovery; restart the service"
        );
        ensure!(
            state.active == 0 || !wait.is_zero(),
            "database busy; maintenance deferred"
        );
        state.maintenance = true;
        while state.active != 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.maintenance = false;
                self.changed.notify_all();
                return Err(anyhow!(
                    "database operations did not drain; maintenance deferred"
                ));
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| anyhow!("database admission lock poisoned"))?
                .0;
        }
        Ok(MaintenancePermit(Arc::clone(self)))
    }
}

pub struct DatabasePermit(Arc<DatabaseGate>);
impl Drop for DatabasePermit {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active -= 1;
        self.0.changed.notify_all();
    }
}
pub struct MaintenancePermit(Arc<DatabaseGate>);
impl Drop for MaintenancePermit {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.maintenance = false;
        self.0.changed.notify_all();
    }
}

pub struct DatabaseReadConnection {
    // Drop the connection before releasing admission, including on unwind.
    connection: Connection,
    _permit: DatabasePermit,
}
impl DatabaseReadConnection {
    pub fn new(connection: Connection, permit: DatabasePermit) -> Self {
        Self {
            connection,
            _permit: permit,
        }
    }
}
impl Deref for DatabaseReadConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.connection
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Fragmentation {
    pub bytes: u64,
    pub max_row_groups: u64,
    pub fragmented: bool,
}

/// Inspect column metadata only; COUNT(*) on fragmented DuckDB tables itself
/// loads hundreds of MiB of deletion vectors and is unsuitable as a probe.
pub fn inspect_fragmentation(
    conn: &Connection,
    bytes: u64,
    config: &DatabaseMaintenanceConfig,
) -> Result<Fragmentation> {
    let mut info = Fragmentation {
        bytes,
        ..Default::default()
    };
    if bytes < config.min_database_mib * 1024 * 1024 {
        return Ok(info);
    }
    let tables = conn
        .prepare("SELECT table_name FROM duckdb_tables() WHERE NOT internal")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for table in tables {
        let (groups, rows): (u64, u64) = conn.query_row(
            "SELECT count(DISTINCT row_group_id), coalesce(sum(count),0)::UBIGINT FROM pragma_storage_info(?) WHERE column_id=0 AND segment_type <> 'VALIDITY'",
            [&table], |r| Ok((r.get(0)?, r.get(1)?)))?;
        info.max_row_groups = info.max_row_groups.max(groups);
        info.fragmented |= groups >= config.fragmented_row_groups && rows / groups.max(1) < 2048;
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maintenance_waits_for_connection_lifetime_and_reopens_admission() {
        let gate = Arc::new(DatabaseGate::default());
        let connection = DatabaseReadConnection::new(
            Connection::open_in_memory().unwrap(),
            gate.enter().unwrap(),
        );
        assert!(gate.maintain(Duration::ZERO).is_err());
        assert!(gate.maintain(Duration::from_millis(5)).is_err());
        drop(connection);
        let exclusive = gate.maintain(Duration::ZERO).unwrap();
        let other = Arc::clone(&gate);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let permit = other.enter().unwrap();
            tx.send(()).unwrap();
            drop(permit);
        });
        assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(exclusive);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
    }
    #[test]
    fn uncertain_recovery_keeps_admission_closed_after_maintenance_exits() {
        let gate = Arc::new(DatabaseGate::default());
        let exclusive = gate.maintain(Duration::ZERO).unwrap();
        gate.require_recovery();
        drop(exclusive);
        assert!(gate.enter().is_err());
        assert!(gate.maintain(Duration::ZERO).is_err());
    }

    #[test]
    fn fragmentation_probe_reads_real_duckdb_metadata() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE items(id BIGINT, value VARCHAR); INSERT INTO items SELECT i, 'v' FROM range(100) t(i)").unwrap();
        let config = DatabaseMaintenanceConfig {
            min_database_mib: 1,
            ..Default::default()
        };
        let info = inspect_fragmentation(&conn, 2 * 1024 * 1024, &config).unwrap();
        assert!(info.max_row_groups > 0);
        assert!(!info.fragmented);
        assert_eq!(
            inspect_fragmentation(&conn, 1, &config)
                .unwrap()
                .max_row_groups,
            0
        );
    }

    #[test]
    fn config_rejects_unbounded_or_too_small_budgets() {
        let mut config = DatabaseMaintenanceConfig::default();
        config.validate().unwrap();
        config.channel_memory_mib = 0;
        assert!(config.validate().is_err());
    }
}
