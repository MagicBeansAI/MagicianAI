//! Durable, scope-owned browser-engine usage analytics.
//!
//! Browser sessions record one row for every real engine command attempt at
//! the subprocess boundary. That placement preserves failed primary launches
//! and successful fallback attempts as separate facts. The read path is backed
//! by SQLite `LIMIT`/`OFFSET`, so UI pagination never loads the full history.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, Transaction};
use serde::Serialize;
use url::Url;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

pub const DEFAULT_BROWSER_ENGINE_PAGE_SIZE: usize = 25;
pub const MAX_BROWSER_ENGINE_PAGE_SIZE: usize = 100;
pub const BROWSER_ENGINE_RETENTION_DAYS: i64 = 90;
pub const BROWSER_ENGINE_MAX_RECORDS: usize = 50_000;

#[derive(Debug, Clone)]
pub struct BrowserEngineAnalyticsContext {
    database_path: PathBuf,
    execution_id: Option<String>,
    task_id: Option<String>,
    work_kind: Option<String>,
    work_id: Option<String>,
}

impl BrowserEngineAnalyticsContext {
    pub fn for_scope(
        storage_root: &Path,
        principal: &str,
        workspace: &str,
        execution_id: Option<String>,
        task_id: Option<String>,
    ) -> Self {
        Self {
            database_path: browser_engine_database_path(storage_root, principal, workspace),
            execution_id: normalized_optional(execution_id),
            task_id: normalized_optional(task_id),
            work_kind: None,
            work_id: None,
        }
    }

    pub fn with_work(mut self, kind: impl Into<String>, id: impl Into<String>) -> Self {
        self.work_kind = normalized_optional(Some(kind.into()));
        self.work_id = normalized_optional(Some(id.into()));
        self
    }

    pub async fn record(&self, input: BrowserEngineUsageInput) {
        let database_path = self.database_path.clone();
        let execution_id = self.execution_id.clone();
        let task_id = self.task_id.clone();
        let work_kind = self.work_kind.clone();
        let work_id = self.work_id.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            record_sync(
                &database_path,
                execution_id,
                task_id,
                work_kind,
                work_id,
                input,
            )
        })
        .await;
        match outcome {
            Ok(Ok(())) => {},
            Ok(Err(error)) => tracing::warn!(
                error = %error,
                "browser-engine analytics write failed; browser work remains authoritative"
            ),
            Err(error) => tracing::warn!(
                error = %error,
                "browser-engine analytics writer task failed; browser work remains authoritative"
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrowserEngineUsageInput {
    pub session_id: String,
    pub engine: String,
    pub fallback_from: Option<String>,
    pub connection_mode: String,
    pub operation: String,
    pub url: Option<String>,
    pub success: bool,
    pub elapsed_ms: u64,
    pub error_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEngineUsageRecord {
    pub id: String,
    pub occurred_at_ms: i64,
    pub session_id: String,
    pub execution_id: Option<String>,
    pub task_id: Option<String>,
    pub work_kind: String,
    pub work_id: String,
    pub engine: String,
    pub fallback_from: Option<String>,
    pub connection_mode: String,
    pub operation: String,
    pub url: Option<String>,
    pub success: bool,
    pub elapsed_ms: u64,
    pub error_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEngineSummary {
    pub engine: String,
    pub attempts: usize,
    pub successes: usize,
    pub failures: usize,
    pub success_rate: f64,
    pub average_elapsed_ms: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEngineUsagePage {
    pub items: Vec<BrowserEngineUsageRecord>,
    pub total_count: usize,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
    pub summary: Vec<BrowserEngineSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct BrowserEngineUsageFilter {
    pub engine: Option<String>,
    pub success: Option<bool>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub fn browser_engine_database_path(
    storage_root: &Path,
    principal: &str,
    workspace: &str,
) -> PathBuf {
    crate::magician_v2::database_owners::database_file_path(
        &ArtifactV2Workspace::new(storage_root),
        principal,
        workspace,
        crate::magician_v2::database_owners::DatabaseOwner::BrowserEngineUsage,
    )
}

/// Store only an HTTP(S) origin + path. Credentials, query strings, and
/// fragments routinely contain tokens or personal search terms and never
/// belong in an analytics row.
pub fn sanitize_browser_analytics_url(candidate: Option<&str>) -> Option<String> {
    let candidate = candidate?.trim();
    let mut parsed = Url::parse(candidate).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

pub async fn list_browser_engine_usage(
    storage_root: &Path,
    principal: &str,
    workspace: &str,
    filter: BrowserEngineUsageFilter,
) -> Result<BrowserEngineUsagePage> {
    let database_path = browser_engine_database_path(storage_root, principal, workspace);
    tokio::task::spawn_blocking(move || list_sync(&database_path, filter))
        .await
        .context("browser-engine analytics reader task failed")?
}

fn normalized_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn open_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create browser analytics dir {}", parent.display()))?;
    }
    let connection = Connection::open(path)
        .with_context(|| format!("open browser analytics database {}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(2))?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS browser_engine_usage (
             id TEXT PRIMARY KEY,
             occurred_at_ms INTEGER NOT NULL,
             session_id TEXT NOT NULL,
             execution_id TEXT,
             task_id TEXT,
             work_kind TEXT NOT NULL,
             work_id TEXT NOT NULL,
             engine TEXT NOT NULL,
             fallback_from TEXT,
             connection_mode TEXT NOT NULL,
             operation TEXT NOT NULL,
             url TEXT,
             success INTEGER NOT NULL,
             elapsed_ms INTEGER NOT NULL,
             error_class TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_browser_engine_usage_newest
             ON browser_engine_usage(occurred_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_browser_engine_usage_engine_outcome
             ON browser_engine_usage(engine, success, occurred_at_ms DESC);",
    )?;
    ensure_work_kind_column(&connection)?;
    Ok(connection)
}

fn ensure_work_kind_column(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(browser_engine_usage)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    if !columns.iter().any(|column| column == "work_kind") {
        connection.execute(
            "ALTER TABLE browser_engine_usage
             ADD COLUMN work_kind TEXT NOT NULL DEFAULT 'browser_session'",
            [],
        )?;
    }
    Ok(())
}

fn record_sync(
    database_path: &Path,
    execution_id: Option<String>,
    task_id: Option<String>,
    work_kind: Option<String>,
    explicit_work_id: Option<String>,
    input: BrowserEngineUsageInput,
) -> Result<()> {
    let connection = open_database(database_path)?;
    let execution_id = normalized_optional(execution_id);
    let task_id = normalized_optional(task_id);
    let explicit_work_id = normalized_optional(explicit_work_id);
    let explicit_work_kind = normalized_optional(work_kind);
    let (work_kind, work_id) = if let Some(task_id) = task_id.as_ref() {
        ("task".to_string(), task_id.clone())
    } else if let Some(execution_id) = execution_id.as_ref() {
        ("execution".to_string(), execution_id.clone())
    } else if let Some(work_id) = explicit_work_id {
        (
            explicit_work_kind.unwrap_or_else(|| "work".to_string()),
            work_id,
        )
    } else {
        ("browser_session".to_string(), input.session_id.clone())
    };
    let occurred_at_ms = chrono::Utc::now().timestamp_millis();
    let elapsed_ms = i64::try_from(input.elapsed_ms).unwrap_or(i64::MAX);
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "INSERT INTO browser_engine_usage (
            id, occurred_at_ms, session_id, execution_id, task_id, work_kind, work_id,
            engine, fallback_from, connection_mode, operation, url, success,
            elapsed_ms, error_class
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            uuid::Uuid::new_v4().to_string(),
            occurred_at_ms,
            input.session_id,
            execution_id,
            task_id,
            work_kind,
            work_id,
            input.engine,
            normalized_optional(input.fallback_from),
            input.connection_mode,
            input.operation,
            sanitize_browser_analytics_url(input.url.as_deref()),
            if input.success { 1_i64 } else { 0_i64 },
            elapsed_ms,
            normalized_optional(input.error_class),
        ],
    )?;
    let newest_rowid = transaction.last_insert_rowid();
    prune_retention(
        &transaction,
        occurred_at_ms,
        BROWSER_ENGINE_MAX_RECORDS,
        newest_rowid,
    )?;
    transaction.commit()?;
    Ok(())
}

fn prune_retention(
    transaction: &Transaction<'_>,
    now_ms: i64,
    max_records: usize,
    newest_rowid: i64,
) -> Result<()> {
    let retention_floor_ms = now_ms.saturating_sub(
        BROWSER_ENGINE_RETENTION_DAYS
            .saturating_mul(24)
            .saturating_mul(60)
            .saturating_mul(60)
            .saturating_mul(1_000),
    );
    transaction.execute(
        "DELETE FROM browser_engine_usage WHERE occurred_at_ms < ?1",
        params![retention_floor_ms],
    )?;
    let oldest_allowed_rowid =
        newest_rowid.saturating_sub(i64::try_from(max_records).unwrap_or(i64::MAX));
    if oldest_allowed_rowid > 0 {
        transaction.execute(
            "DELETE FROM browser_engine_usage WHERE rowid <= ?1",
            params![oldest_allowed_rowid],
        )?;
    }
    Ok(())
}

fn filter_sql(filter: &BrowserEngineUsageFilter) -> (String, Vec<SqlValue>) {
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    if let Some(engine) = filter
        .engine
        .as_deref()
        .map(str::trim)
        .filter(|engine| !engine.is_empty())
    {
        clauses.push("engine = ?".to_string());
        values.push(SqlValue::Text(engine.to_string()));
    }
    if let Some(success) = filter.success {
        clauses.push("success = ?".to_string());
        values.push(SqlValue::Integer(if success { 1_i64 } else { 0_i64 }));
    }
    let sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (sql, values)
}

fn list_sync(path: &Path, filter: BrowserEngineUsageFilter) -> Result<BrowserEngineUsagePage> {
    let limit = filter
        .limit
        .unwrap_or(DEFAULT_BROWSER_ENGINE_PAGE_SIZE)
        .clamp(1, MAX_BROWSER_ENGINE_PAGE_SIZE);
    let offset = filter.offset.unwrap_or(0);
    if !path.exists() {
        return Ok(BrowserEngineUsagePage {
            items: Vec::new(),
            total_count: 0,
            limit,
            offset,
            has_more: false,
            summary: Vec::new(),
        });
    }
    let connection = open_database(path)?;
    let (where_sql, values) = filter_sql(&filter);

    let total_count: i64 = connection.query_row(
        &format!("SELECT COUNT(*) FROM browser_engine_usage{where_sql}"),
        params_from_iter(values.iter()),
        |row| row.get(0),
    )?;

    let mut page_values = values.clone();
    page_values.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    page_values.push(SqlValue::Integer(i64::try_from(offset).unwrap_or(i64::MAX)));
    let limit_index = values.len() + 1;
    let offset_index = values.len() + 2;
    let mut statement = connection.prepare(&format!(
        "SELECT id, occurred_at_ms, session_id, execution_id, task_id, work_kind, work_id,
                engine, fallback_from, connection_mode, operation, url, success,
                elapsed_ms, error_class
         FROM browser_engine_usage{where_sql}
         ORDER BY occurred_at_ms DESC, rowid DESC
         LIMIT ?{limit_index} OFFSET ?{offset_index}"
    ))?;
    let items = statement
        .query_map(params_from_iter(page_values.iter()), |row| {
            Ok(BrowserEngineUsageRecord {
                id: row.get(0)?,
                occurred_at_ms: row.get(1)?,
                session_id: row.get(2)?,
                execution_id: row.get(3)?,
                task_id: row.get(4)?,
                work_kind: row.get(5)?,
                work_id: row.get(6)?,
                engine: row.get(7)?,
                fallback_from: row.get(8)?,
                connection_mode: row.get(9)?,
                operation: row.get(10)?,
                url: row.get(11)?,
                success: row.get::<_, i64>(12)? != 0,
                elapsed_ms: u64::try_from(row.get::<_, i64>(13)?).unwrap_or(0),
                error_class: row.get(14)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut summary_statement = connection.prepare(&format!(
        "SELECT engine, COUNT(*), SUM(success), AVG(elapsed_ms)
         FROM browser_engine_usage{where_sql}
         GROUP BY engine
         ORDER BY COUNT(*) DESC, engine ASC"
    ))?;
    let summary = summary_statement
        .query_map(params_from_iter(values.iter()), |row| {
            let attempts = usize::try_from(row.get::<_, i64>(1)?).unwrap_or(0);
            let successes = usize::try_from(row.get::<_, i64>(2)?).unwrap_or(0);
            Ok(BrowserEngineSummary {
                engine: row.get(0)?,
                attempts,
                successes,
                failures: attempts.saturating_sub(successes),
                success_rate: if attempts == 0 {
                    0.0
                } else {
                    successes as f64 / attempts as f64
                },
                average_elapsed_ms: row.get::<_, f64>(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let total_count = usize::try_from(total_count).unwrap_or(usize::MAX);
    Ok(BrowserEngineUsagePage {
        items,
        total_count,
        limit,
        offset,
        has_more: offset.saturating_add(limit) < total_count,
        summary,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn input(engine: &str, success: bool, url: &str) -> BrowserEngineUsageInput {
        BrowserEngineUsageInput {
            session_id: "session-1".to_string(),
            engine: engine.to_string(),
            fallback_from: (engine == "bundled_chrome").then(|| "cloak-browser".to_string()),
            connection_mode: "headless".to_string(),
            operation: "open".to_string(),
            url: Some(url.to_string()),
            success,
            elapsed_ms: if success { 12 } else { 30 },
            error_class: (!success).then(|| "capacity_limit".to_string()),
        }
    }

    #[test]
    fn analytics_url_removes_credentials_query_and_fragment() {
        assert_eq!(
            sanitize_browser_analytics_url(Some(
                "https://user:secret@example.com/flights/AI123?token=abc#private"
            ))
            .as_deref(),
            Some("https://example.com/flights/AI123")
        );
        assert_eq!(sanitize_browser_analytics_url(Some("about:blank")), None);
    }

    #[tokio::test]
    async fn records_real_attempts_and_pages_them_on_the_server() {
        let temp = tempfile::tempdir().unwrap();
        let context = BrowserEngineAnalyticsContext::for_scope(
            temp.path(),
            "owner",
            "default",
            Some("exec-1".to_string()),
            Some("task-1".to_string()),
        );
        context
            .record(input(
                "cloak-browser",
                false,
                "https://example.com/one?secret=yes",
            ))
            .await;
        context
            .record(input(
                "bundled_chrome",
                true,
                "https://example.com/one?secret=yes",
            ))
            .await;
        context
            .record(input("lightpanda", true, "https://example.com/two"))
            .await;

        let first = list_browser_engine_usage(
            temp.path(),
            "owner",
            "default",
            BrowserEngineUsageFilter {
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(first.total_count, 3);
        assert_eq!(first.items.len(), 2);
        assert!(first.has_more);
        assert!(first.items.iter().all(|row| row.work_id == "task-1"));
        assert!(first
            .items
            .iter()
            .all(|row| { row.url.as_deref().is_none_or(|url| !url.contains("secret")) }));

        let failures = list_browser_engine_usage(
            temp.path(),
            "owner",
            "default",
            BrowserEngineUsageFilter {
                success: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(failures.total_count, 1);
        assert_eq!(failures.items[0].engine, "cloak-browser");
        assert_eq!(
            failures.items[0].error_class.as_deref(),
            Some("capacity_limit")
        );
        assert_eq!(failures.summary[0].success_rate, 0.0);
    }

    #[test]
    fn retention_removes_expired_rows_and_keeps_only_the_newest_cap() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("usage.sqlite3");
        let mut connection = open_database(&path).unwrap();
        for timestamp in [1_i64, 2, 3, 4] {
            connection
                .execute(
                    "INSERT INTO browser_engine_usage (
                        id, occurred_at_ms, session_id, work_kind, work_id, engine,
                        connection_mode, operation, success, elapsed_ms
                     ) VALUES (?1, ?2, 'session', 'test', 'work', 'lightpanda',
                               'headless', 'open', 1, 1)",
                    params![format!("row-{timestamp}"), timestamp],
                )
                .unwrap();
        }
        let transaction = connection.transaction().unwrap();
        prune_retention(&transaction, 4, 2, 4).unwrap();
        transaction.commit().unwrap();

        let ids = connection
            .prepare("SELECT id FROM browser_engine_usage ORDER BY occurred_at_ms")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids, vec!["row-3", "row-4"]);

        connection
            .execute(
                "UPDATE browser_engine_usage SET occurred_at_ms = 0 WHERE id = 'row-3'",
                [],
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        let after_horizon_ms = BROWSER_ENGINE_RETENTION_DAYS * 24 * 60 * 60 * 1_000 + 1;
        prune_retention(&transaction, after_horizon_ms, 10, 4).unwrap();
        transaction.commit().unwrap();
        let remaining: i64 = connection
            .query_row("SELECT COUNT(*) FROM browser_engine_usage", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 1);
    }
}
