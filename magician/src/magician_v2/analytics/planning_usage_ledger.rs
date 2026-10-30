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

/// Planning totals from the governed files for the requested UTC days.
/// Read bounded groups so DuckDB never binds metadata for a whole raw lake.
/// A partial scan is an error: the caller retains its retry marker.
pub fn planning_usage_by_execution(
    workspace: crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    scope: magicllm::LlmScope,
    days: Vec<String>,
    wanted: HashSet<String>,
) -> Result<BTreeMap<String, PlanningUsage>> {
    use super::duckdb_safety::{
        configure_analytics_connection_checked, try_analytics_duckdb_guard_for,
    };
    use super::llm_fact_compactor::governed_dataset_sources_in_date_range;
    use super::llm_fact_registry::LlmCanonicalDataset;
    use super::parquet_maintenance::{partition_sources, PartitionedDataset};
    use std::time::{Duration, Instant};

    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    let connection = duckdb::Connection::open_in_memory()?;
    configure_analytics_connection_checked(&connection, "planning_usage_backfill")?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut totals = BTreeMap::new();
    for day in days {
        anyhow::ensure!(
            Instant::now() < deadline,
            "planning usage backfill deadline exceeded"
        );
        let date = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d")?;
        let files = {
            let _guard = try_analytics_duckdb_guard_for(Duration::from_secs(5))
                .ok_or_else(|| anyhow::anyhow!("analytics busy; defer planning usage backfill"))?;
            let dir = workspace
                .analytics_llm_calls_root(&scope.principal, &scope.workspace)
                .join(format!("dt={day}"));
            let mut files = if dir.is_dir() {
                partition_sources(&dir, PartitionedDataset::LegacyLlmCalls)?
            } else {
                Vec::new()
            };
            files.extend(
                governed_dataset_sources_in_date_range(
                    &workspace,
                    &scope,
                    LlmCanonicalDataset::Calls,
                    Some(date),
                    Some(date),
                )?
                .into_iter()
                .flat_map(|source| source.files),
            );
            files.sort();
            files.dedup();
            files
        };
        let usage = read_day_files(&connection, &files, &wanted, deadline)?;
        for (execution_id, usage) in usage {
            totals
                .entry(execution_id)
                .and_modify(|sum| merge(sum, &usage))
                .or_insert(usage);
        }
    }
    Ok(totals)
}

fn read_day_files(
    connection: &duckdb::Connection,
    files: &[PathBuf],
    wanted: &HashSet<String>,
    deadline: std::time::Instant,
) -> Result<BTreeMap<String, PlanningUsage>> {
    use super::duckdb_safety::{
        run_analytics_query_with_interrupt_timeout, try_analytics_duckdb_guard_for,
    };
    use std::time::{Duration, Instant};
    const FILES_PER_QUERY: usize = 64;
    let wanted_sql = wanted
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let mut calls: BTreeMap<(String, String), PlanningUsage> = BTreeMap::new();
    for (batch, files) in files.chunks(FILES_PER_QUERY).enumerate() {
        anyhow::ensure!(
            Instant::now() < deadline,
            "planning usage backfill deadline exceeded"
        );
        let _guard = try_analytics_duckdb_guard_for(Duration::from_secs(5))
            .ok_or_else(|| anyhow::anyhow!("analytics busy; defer planning usage backfill"))?;
        let timeout =
            Duration::from_secs(3).min(deadline.saturating_duration_since(Instant::now()));
        let result = run_analytics_query_with_interrupt_timeout(
            connection,
            timeout,
            || -> Result<()> {
                let paths = files
                    .iter()
                    .map(|path| format!("'{}'", path.to_string_lossy().replace('\'', "''")))
                    .collect::<Vec<_>>()
                    .join(",");
                let source = format!("read_parquet([{paths}], union_by_name = true)");
                let columns = parquet_columns(connection, &source)?;
                if !columns.contains("execution_id") {
                    return Ok(());
                }
                let key = if columns.contains("llm_call_id") {
                    "CAST(llm_call_id AS VARCHAR)".to_owned()
                } else {
                    format!("concat('legacy-batch-{batch}:', row_number() OVER ())")
                };
                let column = |name: &str, fallback: &str| {
                    if columns.contains(name) {
                        name.to_string()
                    } else {
                        fallback.to_string()
                    }
                };
                let sql = format!(
                "SELECT execution_id, call_key, MAX(COALESCE(input_tokens,0))::DOUBLE, \
                 MAX(COALESCE(output_tokens,0))::DOUBLE, MAX(cost_usd)::DOUBLE, MAX(timestamp_ms)::BIGINT \
                 FROM (SELECT execution_id, {key} AS call_key, {input} AS input_tokens, \
                 {output} AS output_tokens, {cost} AS cost_usd, {timestamp} AS timestamp_ms \
                 FROM {source} WHERE execution_id IN ({wanted_sql})) \
                 WHERE call_key IS NOT NULL GROUP BY execution_id,call_key",
                input=column("input_tokens", "NULL"), output=column("output_tokens", "NULL"),
                cost=column("cost_usd", "CAST(NULL AS DOUBLE)"), timestamp=column("timestamp_ms", "CAST(NULL AS BIGINT)"),
            );
                let mut statement = connection.prepare(&sql)?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                        PlanningUsage {
                            calls: 1,
                            input_tokens: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0).max(0.0)
                                as u64,
                            output_tokens: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0).max(0.0)
                                as u64,
                            cost_usd: row.get(4)?,
                            last_call_ms: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                        },
                    ))
                })?;
                for row in rows {
                    let (key, usage) = row?;
                    calls
                        .entry(key)
                        .and_modify(|prior| {
                            prior.input_tokens = prior.input_tokens.max(usage.input_tokens);
                            prior.output_tokens = prior.output_tokens.max(usage.output_tokens);
                            prior.cost_usd = match (prior.cost_usd, usage.cost_usd) {
                                (Some(a), Some(b)) => Some(a.max(b)),
                                (a, b) => a.or(b),
                            };
                            prior.last_call_ms = prior.last_call_ms.max(usage.last_call_ms);
                        })
                        .or_insert(usage);
                }
                Ok(())
            },
        );
        match result {
            Ok(()) => {},
            Err(super::duckdb_safety::AnalyticsDuckDbQueryError::Query(error)) => {
                return Err(error)
            },
            Err(super::duckdb_safety::AnalyticsDuckDbQueryError::TimedOut(_)) => {
                anyhow::bail!("planning usage batch timed out")
            },
        }
        drop(_guard);
        // Give foreground analytics readers an admission opportunity.
        std::thread::sleep(Duration::from_millis(25));
    }
    let mut totals = BTreeMap::new();
    for ((execution_id, _), usage) in calls {
        totals
            .entry(execution_id)
            .and_modify(|sum| merge(sum, &usage))
            .or_insert(usage);
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
    fn planning_usage_bounded_batches_deduplicate_calls_across_files() {
        let temp = tempfile::tempdir().unwrap();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        super::super::duckdb_safety::configure_analytics_connection_checked(&conn, "test").unwrap();
        let first = temp.path().join("first.parquet");
        conn.execute_batch(&format!(
            "COPY (SELECT 'planexec_wanted' AS execution_id,'call-one' AS llm_call_id,10 AS input_tokens,2 AS output_tokens,0.5 AS cost_usd,7 AS timestamp_ms UNION ALL SELECT 'planexec_other','other',900,900,90,8) TO '{}' (FORMAT PARQUET)",
            first.display(),
        )).unwrap();
        let mut files = vec![first.clone()];
        for index in 1..130 {
            let copy = temp.path().join(format!("copy-{index}.parquet"));
            std::fs::copy(&first, &copy).unwrap();
            files.push(copy);
        }
        let result = read_day_files(
            &conn,
            &files,
            &HashSet::from(["planexec_wanted".to_owned()]),
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result["planexec_wanted"],
            PlanningUsage {
                calls: 1,
                input_tokens: 10,
                output_tokens: 2,
                cost_usd: Some(0.5),
                last_call_ms: 7,
            }
        );
        assert!(read_day_files(
            &conn,
            &files,
            &HashSet::from(["planexec_wanted".to_owned()]),
            std::time::Instant::now() - std::time::Duration::from_secs(1)
        )
        .is_err());
    }

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
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(tmp.path()),
            magicllm::LlmScope::new("owner", "workspace"),
            vec!["2026-10-01".to_string()],
            HashSet::from(["planexec_1".to_string()]),
        )
        .unwrap();
        assert!(totals.is_empty());
    }
}
