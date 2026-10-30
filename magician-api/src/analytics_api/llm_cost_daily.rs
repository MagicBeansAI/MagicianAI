//! `llm_cost_daily`: LLM spend summed per day, task, chat session and model.
//!
//! Every `llm_calls` query rebuilds the reconciled relation over its whole
//! window, so a lifetime total cost grows with history — about 19k calls a day
//! — and had reached several seconds. A day's spend never changes once its
//! files stop changing, so this summarises each day once, keeps the summary in
//! memory keyed by that day's files, and recomputes only a day whose files
//! moved (today, mostly). A lifetime query then sums a few hundred rows a day
//! instead of every call.
//!
//! A day is summarised over its own partition plus its neighbours', keeping
//! only its own rows. The reconcile drops a canonical call whose legacy batch
//! row also exists, and the two copies of a call can land in adjacent day
//! partitions; summarising a day alone counted ~0.06% of calls twice.
//!
//! The relation carries no content: identifiers, model names, counts and sums.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::Instant,
};

use chrono::NaiveDate;
use magician::magician_v2::analytics::duckdb_safety::configure_analytics_read_connection_checked;
use magician::magician_v2::analytics::llm_fact_compactor::governed_dataset_sources_in_date_range;
use magician::magician_v2::analytics::llm_fact_registry::LlmCanonicalDataset;
use magician::magician_v2::analytics::parquet_maintenance::{
    partition_sources, PartitionedDataset,
};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magicllm::LlmScope;

use super::duckdb_parquet_source_sql;
use magician::magician_v2::analytics::legacy_llm_compat::install_legacy_llm_views_for_daily_summary;

/// The relation name client SQL reads.
pub(super) const LLM_COST_DAILY_RELATION: &str = "llm_cost_daily";

const CREATE_SQL: &str = "CREATE TABLE llm_cost_daily (
    dt VARCHAR,
    task_id VARCHAR,
    chat_session_id VARCHAR,
    provider VARCHAR,
    model VARCHAR,
    calls BIGINT,
    cost_usd DOUBLE,
    input_tokens BIGINT,
    output_tokens BIGINT,
    reasoning_tokens BIGINT,
    cache_read_tokens BIGINT,
    cache_creation_tokens BIGINT,
    first_call_ms BIGINT,
    last_call_ms BIGINT,
    root_execution_id VARCHAR,
    measured_calls BIGINT,
    priced_calls BIGINT,
    token_calls BIGINT,
    cache_read_calls BIGINT,
    cache_write_calls BIGINT
)";

#[derive(Debug, Clone, PartialEq)]
struct CostDailyRow {
    task_id: Option<String>,
    chat_session_id: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    calls: i64,
    cost_usd: f64,
    input_tokens: i64,
    output_tokens: i64,
    reasoning_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    first_call_ms: Option<i64>,
    last_call_ms: Option<i64>,
    root_execution_id: Option<String>,
    measured_calls: i64,
    priced_calls: i64,
    token_calls: i64,
    cache_read_calls: i64,
    cache_write_calls: i64,
}

type DayKey = (String, String, NaiveDate);

/// One summary per scope and day, with the fingerprint of the files it was
/// computed from. Bounded by the days on disk.
static DAY_SUMMARIES: LazyLock<Mutex<HashMap<DayKey, (u64, Arc<Vec<CostDailyRow>>)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Install `llm_cost_daily` into `conn`. The caller holds the analytics
/// DuckDB guard and disables external access afterwards, as for `llm_calls`.
pub(super) fn install_llm_cost_daily(
    conn: &duckdb::Connection,
    layout: &ArtifactV2Workspace,
    scope: &LlmScope,
    llm_calls_root: &Path,
    deadline: Instant,
) -> anyhow::Result<()> {
    conn.execute_batch(CREATE_SQL)
        .map_err(|error| anyhow::anyhow!("create llm_cost_daily: {error}"))?;
    let days = partition_days(llm_calls_root)?;
    let mut day_files = HashMap::new();
    for day in &days {
        anyhow::ensure!(
            Instant::now() < deadline,
            "daily usage preparation deadline exceeded"
        );
        day_files.insert(
            *day,
            DayFiles::governed(layout, scope, llm_calls_root, *day)?,
        );
    }
    let mut appender = conn
        .appender(LLM_COST_DAILY_RELATION)
        .map_err(|error| anyhow::anyhow!("open llm_cost_daily appender: {error}"))?;
    for (index, day) in days.iter().enumerate() {
        let window: Vec<&DayFiles> = days[index.saturating_sub(1)..(index + 2).min(days.len())]
            .iter()
            .filter_map(|neighbour| day_files.get(neighbour))
            .collect();
        anyhow::ensure!(
            Instant::now() < deadline,
            "daily usage preparation deadline exceeded"
        );
        let rows = day_summary(scope, *day, &window, deadline)?;
        let dt = day.format("%Y-%m-%d").to_string();
        for row in rows.iter() {
            appender
                .append_row(duckdb::params![
                    dt,
                    row.task_id,
                    row.chat_session_id,
                    row.provider,
                    row.model,
                    row.calls,
                    row.cost_usd,
                    row.input_tokens,
                    row.output_tokens,
                    row.reasoning_tokens,
                    row.cache_read_tokens,
                    row.cache_creation_tokens,
                    row.first_call_ms,
                    row.last_call_ms,
                    row.root_execution_id,
                    row.measured_calls,
                    row.priced_calls,
                    row.token_calls,
                    row.cache_read_calls,
                    row.cache_write_calls,
                ])
                .map_err(|error| anyhow::anyhow!("append llm_cost_daily row: {error}"))?;
        }
    }
    appender
        .flush()
        .map_err(|error| anyhow::anyhow!("flush llm_cost_daily: {error}"))?;
    Ok(())
}

/// The governed read set of one day partition.
struct DayFiles {
    calls: Vec<PathBuf>,
    attempts: Vec<PathBuf>,
}

impl DayFiles {
    fn governed(
        layout: &ArtifactV2Workspace,
        scope: &LlmScope,
        llm_calls_root: &Path,
        day: NaiveDate,
    ) -> anyhow::Result<Self> {
        let canonical = |dataset| -> anyhow::Result<Vec<PathBuf>> {
            Ok(governed_dataset_sources_in_date_range(
                layout,
                scope,
                dataset,
                Some(day),
                Some(day),
            )?
            .into_iter()
            .flat_map(|source| source.files)
            .collect())
        };
        let mut calls = canonical(LlmCanonicalDataset::Calls)?;
        let partition = llm_calls_root.join(format!("dt={}", day.format("%Y-%m-%d")));
        if partition.is_dir() {
            calls.extend(partition_sources(
                &partition,
                PartitionedDataset::LegacyLlmCalls,
            )?);
        }
        calls.sort();
        calls.dedup();
        Ok(Self {
            calls,
            attempts: canonical(LlmCanonicalDataset::ProviderAttempts)?,
        })
    }
}

fn day_summary(
    scope: &LlmScope,
    day: NaiveDate,
    window: &[&DayFiles],
    deadline: Instant,
) -> anyhow::Result<Arc<Vec<CostDailyRow>>> {
    let calls: Vec<PathBuf> = window
        .iter()
        .flat_map(|files| files.calls.clone())
        .collect();
    let attempts: Vec<PathBuf> = window
        .iter()
        .flat_map(|files| files.attempts.clone())
        .collect();
    if calls.is_empty() {
        return Ok(Arc::new(Vec::new()));
    }
    let fingerprint = files_fingerprint(calls.iter().chain(attempts.iter()));
    let key = (scope.principal.clone(), scope.workspace.clone(), day);
    if let Some((cached, rows)) = lock_summaries().get(&key) {
        if *cached == fingerprint {
            return Ok(Arc::clone(rows));
        }
    }
    let rows = Arc::new(compute_day(scope, day, &calls, &attempts, deadline)?);
    lock_summaries().insert(key, (fingerprint, Arc::clone(&rows)));
    Ok(rows)
}

fn lock_summaries() -> std::sync::MutexGuard<'static, HashMap<DayKey, (u64, Arc<Vec<CostDailyRow>>)>>
{
    match DAY_SUMMARIES.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Summarise one day on its own connection, through the same reconciled
/// `llm_calls` relation every other LLM query reads.
fn compute_day(
    scope: &LlmScope,
    day: NaiveDate,
    calls: &[PathBuf],
    attempts: &[PathBuf],
    deadline: Instant,
) -> anyhow::Result<Vec<CostDailyRow>> {
    let conn = duckdb::Connection::open_in_memory()
        .map_err(|error| anyhow::anyhow!("open in-memory DuckDB: {error}"))?;
    configure_analytics_read_connection_checked(&conn, "llm_cost_daily")
        .map_err(|error| anyhow::anyhow!("configure conservative DuckDB limits: {error}"))?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    anyhow::ensure!(
        !remaining.is_zero(),
        "daily usage preparation deadline exceeded"
    );
    super::run_analytics_operation_with_interrupt_timeout(&conn, remaining, || {
        let attempts_sql = (!attempts.is_empty()).then(|| duckdb_parquet_source_sql(attempts));
        install_legacy_llm_views_for_daily_summary(
            &conn,
            &duckdb_parquet_source_sql(calls),
            attempts_sql.as_deref(),
            &scope.principal,
            &scope.workspace,
        )?;
        let mut statement = conn
            .prepare(
                "SELECT task_id, chat_session_id, provider, model, COUNT(*)::BIGINT, \
                        COALESCE(SUM(cost_usd), 0)::DOUBLE, \
                        COALESCE(SUM(input_tokens), 0)::BIGINT, \
                        COALESCE(SUM(output_tokens), 0)::BIGINT, \
                        COALESCE(SUM(reasoning_tokens), 0)::BIGINT, \
                        COALESCE(SUM(cache_read_tokens), 0)::BIGINT, \
                        COALESCE(SUM(cache_creation_tokens), 0)::BIGINT, \
                        MIN(timestamp_ms)::BIGINT, MAX(timestamp_ms)::BIGINT, \
                        COALESCE(root_execution_id, execution_id), \
                        COUNT(*) FILTER (WHERE (COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate') AND (input_tokens IS NOT NULL OR output_tokens IS NOT NULL OR cost_usd IS NOT NULL))::BIGINT, \
                        COUNT(*) FILTER (WHERE (COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate') AND cost_usd IS NOT NULL)::BIGINT, \
                        COUNT(*) FILTER (WHERE (COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate') AND (input_tokens IS NOT NULL OR output_tokens IS NOT NULL))::BIGINT, \
                        COUNT(*) FILTER (WHERE cache_read_tokens IS NOT NULL)::BIGINT, \
                        COUNT(*) FILTER (WHERE cache_creation_tokens IS NOT NULL)::BIGINT \
                 FROM llm_calls WHERE CAST(dt AS VARCHAR) = ? \
                 GROUP BY 1, 2, 3, 4, 14",
            )
            .map_err(|error| anyhow::anyhow!("prepare llm_cost_daily summary: {error}"))?;
        let rows = statement
            .query_map([day.format("%Y-%m-%d").to_string()], |row| {
                Ok(CostDailyRow {
                    task_id: row.get(0)?,
                    chat_session_id: row.get(1)?,
                    provider: row.get(2)?,
                    model: row.get(3)?,
                    calls: row.get(4)?,
                    cost_usd: row.get(5)?,
                    input_tokens: row.get(6)?,
                    output_tokens: row.get(7)?,
                    reasoning_tokens: row.get(8)?,
                    cache_read_tokens: row.get(9)?,
                    cache_creation_tokens: row.get(10)?,
                    first_call_ms: row.get(11)?,
                    last_call_ms: row.get(12)?,
                    root_execution_id: row.get(13)?,
                    measured_calls: row.get(14)?,
                    priced_calls: row.get(15)?,
                    token_calls: row.get(16)?,
                    cache_read_calls: row.get(17)?,
                    cache_write_calls: row.get(18)?,
                })
            })
            .map_err(|error| anyhow::anyhow!("summarise llm_cost_daily: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| anyhow::anyhow!("read llm_cost_daily row: {error}"))
    })
}

/// Day partitions under the `llm_calls` root, oldest first.
fn partition_days(root: &Path) -> anyhow::Result<Vec<NaiveDate>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut days = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(day) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("dt="))
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
        {
            days.push(day);
        }
    }
    days.sort();
    Ok(days)
}

/// Path, length and modification time of every file a summary read. A
/// compaction, a new raw file or a rewritten object all change it.
fn files_fingerprint<'a>(files: impl Iterator<Item = &'a PathBuf>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for file in files {
        file.hash(&mut hasher);
        if let Ok(metadata) = std::fs::metadata(file) {
            metadata.len().hash(&mut hasher);
            metadata.modified().ok().hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Whether `sql` names `relation` as a whole identifier (case-insensitive).
pub(super) fn sql_mentions_relation(sql: &str, relation: &str) -> bool {
    let haystack = sql.to_ascii_lowercase();
    let needle = relation.to_ascii_lowercase();
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(found) = haystack[start..].find(&needle) {
        let at = start + found;
        let end = at + needle.len();
        let before = at == 0 || !is_ident(bytes[at - 1]);
        let after = end >= bytes.len() || !is_ident(bytes[end]);
        if before && after {
            return true;
        }
        start = at + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relation_is_matched_only_as_a_whole_identifier() {
        assert!(sql_mentions_relation(
            "SELECT * FROM llm_cost_daily",
            "llm_cost_daily"
        ));
        assert!(sql_mentions_relation(
            "select 1 from LLM_COST_DAILY d",
            "llm_cost_daily"
        ));
        assert!(!sql_mentions_relation(
            "SELECT * FROM llm_cost_daily_x",
            "llm_cost_daily"
        ));
        assert!(!sql_mentions_relation(
            "SELECT * FROM llm_calls",
            "llm_cost_daily"
        ));
        assert!(sql_mentions_relation(
            "WITH a AS (SELECT 1) SELECT * FROM llm_calls",
            "llm_calls"
        ));
    }

    #[test]
    fn partition_days_are_listed_oldest_first_and_others_ignored() {
        let root = tempfile::tempdir().unwrap();
        for name in ["dt=2026-10-02", "dt=2026-10-01", "not-a-day", "dt=garbage"] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
        }
        std::fs::write(
            root.path().join("dt=2026-10-03"),
            b"a file, not a partition",
        )
        .unwrap();
        assert_eq!(
            partition_days(root.path()).unwrap(),
            vec![
                NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            ]
        );
    }

    #[test]
    fn the_fingerprint_moves_when_a_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.parquet");
        std::fs::write(&file, b"one").unwrap();
        let files = vec![file.clone()];
        let before = files_fingerprint(files.iter());
        assert_eq!(before, files_fingerprint(files.iter()));
        std::fs::write(&file, b"one two").unwrap();
        assert_ne!(before, files_fingerprint(files.iter()));
    }
}
