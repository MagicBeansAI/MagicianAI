//! Explicit App-store maintenance on the owner's authenticated SQLCipher handle.
//! The registry drains pooled readers/writers before entering this module.
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppStoreMaintenanceAction {
    Verify,
    Optimize,
    Reclaim,
}

impl AppStoreMaintenanceAction {
    pub fn confirmation(self) -> &'static str {
        match self {
            Self::Verify => "CHECK APP DATABASE",
            Self::Optimize => "OPTIMIZE APP DATABASE",
            Self::Reclaim => "RECLAIM APP DATABASE",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AppStoreMaintenanceReport {
    pub principal: String,
    pub workspace: String,
    pub relative_path: &'static str,
    pub operation: AppStoreMaintenanceAction,
    pub encrypted: bool,
    pub integrity_ok: bool,
    pub completed_at: String,
    pub duration_ms: u64,
    pub database_bytes_before: u64,
    pub database_bytes_after: u64,
    pub wal_bytes_after: u64,
    pub shm_bytes_after: u64,
    pub page_count: u64,
    pub free_pages: u64,
    pub checkpoint_busy: bool,
}

pub(super) struct MaintenanceOutcome {
    pub page_count: u64,
    pub free_pages: u64,
    pub checkpoint_busy: bool,
}

/// The registry supplies its already authenticated encrypted handle. Verify
/// SQLCipher support; return no SQL text, key material or logical App rows.
pub(super) fn maintain(
    connection: &Connection,
    action: AppStoreMaintenanceAction,
) -> rusqlite::Result<MaintenanceOutcome> {
    if !connection.is_autocommit() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let cipher: String = connection.pragma_query_value(None, "cipher_version", |row| row.get(0))?;
    if cipher.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    verify_integrity(connection)?;
    let mut checkpoint_busy = false;
    if action != AppStoreMaintenanceAction::Verify {
        // A competing external reader can prevent truncation. Report that
        // condition accurately; never claim its WAL bytes have been reclaimed.
        let busy: i64 =
            connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        checkpoint_busy = busy != 0;
        if action == AppStoreMaintenanceAction::Reclaim {
            if checkpoint_busy {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("App database checkpoint is held by an external reader".to_owned()),
                ));
            }
            // SQLite's transactional VACUUM keeps the existing encrypted
            // connection/key and logical rows; no plaintext copy or file swap.
            connection.execute_batch("VACUUM")?;
        }
        connection.execute_batch("PRAGMA optimize")?;
        let busy: i64 =
            connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        checkpoint_busy |= busy != 0;
        verify_integrity(connection)?;
    }
    Ok(MaintenanceOutcome {
        page_count: connection.pragma_query_value(None, "page_count", |row| row.get(0))?,
        free_pages: connection.pragma_query_value(None, "freelist_count", |row| row.get(0))?,
        checkpoint_busy,
    })
}

fn verify_integrity(connection: &Connection) -> rusqlite::Result<()> {
    let mut cipher = connection.prepare("PRAGMA cipher_integrity_check")?;
    if cipher.query([])?.next()?.is_some() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some("App database encrypted-page integrity check failed".to_owned()),
        ));
    }
    let mut integrity = connection.prepare("PRAGMA integrity_check")?;
    let mut rows = integrity.query([])?;
    let ok = match rows.next()? {
        Some(row) => row.get::<_, String>(0)? == "ok",
        None => false,
    };
    if !ok || rows.next()?.is_some() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some("App database structural integrity check failed".to_owned()),
        ));
    }
    Ok(())
}
