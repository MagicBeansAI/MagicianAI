//! Planning model usage per planning execution, read from the LLM call
//! ledger's parquet. Used by the planning-usage backfill
//! (`progress_channel_seam::planning_usage_backfill`); it lives here because
//! raw parquet reads belong to the reviewed analytics tree.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;

/// A planning execution's totals from the ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanningUsage {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
    pub last_call_ms: i64,
}

/// Ledger totals per planning execution, from the call parquet of the
/// given UTC days only, read directly with DuckDB: the governed analytics
/// reader caps a query at 10 s, which a view over weeks of partitions
/// exceeds. A call can have several rows (legacy, started, completed, and
/// a compacted copy of a batch), so each `llm_call_id` is counted once, with
/// its largest token and cost values.
///
/// `calls_root` is the scope's `llm_calls` root; `days` are `YYYY-MM-DD`
/// partitions; only executions in `wanted` are returned. Blocking: call it
/// from `spawn_blocking`.
pub fn planning_usage_by_execution(
    calls_root: PathBuf,
    days: Vec<String>,
    wanted: HashSet<String>,
) -> Result<BTreeMap<String, PlanningUsage>> {
    let connection = duckdb::Connection::open_in_memory()?;
    let mut totals: BTreeMap<String, PlanningUsage> = BTreeMap::new();
    for day in days {
        let dir = calls_root.join(format!("dt={day}"));
        if !dir.is_dir() {
            continue;
        }
        let glob = format!("{}/**/*.parquet", dir.display()).replace('\'', "''");
        let source = format!("read_parquet('{glob}', union_by_name = true)");
        let columns = match parquet_columns(&connection, &source) {
            Ok(columns) => columns,
            Err(error) => {
                tracing::warn!(%day, %error, "[PLANNING-USAGE-LEDGER] unreadable call partition skipped");
                continue;
            },
        };
        if !columns.contains("execution_id") {
            continue;
        }
        // Partitions written before per-call ids hold one row per call; later
        // ones repeat a call across lifecycle rows, so those are collapsed.
        let call_key = if columns.contains("llm_call_id") {
            "llm_call_id"
        } else {
            "row_number() OVER ()"
        };
        let column = |name: &str, fallback: &str| {
            if columns.contains(name) {
                name.to_string()
            } else {
                fallback.to_string()
            }
        };
        let sql = format!(
            "SELECT execution_id, count(*) AS calls, \
             CAST(sum(input_tokens) AS DOUBLE) AS input_tokens, \
             CAST(sum(output_tokens) AS DOUBLE) AS output_tokens, \
             CAST(sum(cost_usd) AS DOUBLE) AS cost_usd, \
             CAST(max(last_ms) AS BIGINT) AS last_call_ms FROM ( \
               SELECT execution_id, call_key, \
               max(coalesce(input_tokens, 0)) AS input_tokens, \
               max(coalesce(output_tokens, 0)) AS output_tokens, \
               max(cost_usd) AS cost_usd, max(timestamp_ms) AS last_ms FROM ( \
                 SELECT execution_id, {call_key} AS call_key, \
                 {input} AS input_tokens, {output} AS output_tokens, \
                 {cost} AS cost_usd, {timestamp} AS timestamp_ms \
                 FROM {source} WHERE execution_id LIKE 'planexec_%') \
               WHERE call_key IS NOT NULL \
               GROUP BY execution_id, call_key) \
             GROUP BY execution_id",
            input = column("input_tokens", "NULL"),
            output = column("output_tokens", "NULL"),
            cost = column("cost_usd", "CAST(NULL AS DOUBLE)"),
            timestamp = column("timestamp_ms", "CAST(NULL AS BIGINT)"),
        );
        let mut statement = match connection.prepare(&sql) {
            Ok(statement) => statement,
            Err(error) => {
                tracing::warn!(%day, %error, "[PLANNING-USAGE-LEDGER] call partition skipped");
                continue;
            },
        };
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PlanningUsage {
                    calls: row.get::<_, i64>(1)?.max(0) as u64,
                    input_tokens: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0).max(0.0) as u64,
                    output_tokens: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0).max(0.0) as u64,
                    cost_usd: row.get::<_, Option<f64>>(4)?,
                    last_call_ms: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                },
            ))
        })?;
        for row in rows {
            let (execution_id, usage) = row?;
            if !wanted.contains(&execution_id) {
                continue;
            }
            totals
                .entry(execution_id)
                .and_modify(|sum| merge(sum, &usage))
                .or_insert(usage);
        }
    }
    Ok(totals)
}

/// The column names a parquet source exposes.
fn parquet_columns(connection: &duckdb::Connection, source: &str) -> Result<HashSet<String>> {
    let mut statement = connection.prepare(&format!("DESCRIBE SELECT * FROM {source}"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(0))?;
    Ok(names.collect::<Result<HashSet<_>, _>>()?)
}

pub fn merge(sum: &mut PlanningUsage, more: &PlanningUsage) {
    sum.calls += more.calls;
    sum.input_tokens += more.input_tokens;
    sum.output_tokens += more.output_tokens;
    sum.cost_usd = match (sum.cost_usd, more.cost_usd) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    };
    sum.last_call_ms = sum.last_call_ms.max(more.last_call_ms);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_add_up() {
        let mut sum = PlanningUsage {
            calls: 2,
            input_tokens: 10,
            output_tokens: 1,
            cost_usd: None,
            last_call_ms: 7,
        };
        merge(
            &mut sum,
            &PlanningUsage {
                calls: 3,
                input_tokens: 20,
                output_tokens: 2,
                cost_usd: Some(0.5),
                last_call_ms: 9,
            },
        );
        assert_eq!((sum.calls, sum.input_tokens, sum.output_tokens), (5, 30, 3));
        assert_eq!(sum.cost_usd, Some(0.5));
        assert_eq!(sum.last_call_ms, 9);
    }

    #[test]
    fn a_day_with_no_partition_reads_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let totals = planning_usage_by_execution(
            tmp.path().to_path_buf(),
            vec!["2026-10-01".to_string()],
            HashSet::from(["planexec_1".to_string()]),
        )
        .unwrap();
        assert!(totals.is_empty());
    }
}
