//! Content-free compatibility boundary for the pre-canonical `llm_calls` lake.
//!
//! Phase 2 keeps the historical row-per-call dataset readable while canonical
//! facts are adopted. This module is the single boundary shared by REST and
//! `internal_data`: callers can query only the projected compatibility views,
//! never the raw Parquet relation or restricted legacy content columns.

use std::{collections::HashSet, ops::ControlFlow};

use anyhow::{anyhow, Result};
use duckdb::Connection;
use sqlparser::ast::{
    BinaryOperator, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments,
    Interval, ObjectName, Query, Select, SetExpr, Statement, TableFactor, Value, Visit, Visitor,
};

use super::llm_sql_guard::{
    is_read_only_select_query, is_read_only_select_query_node, parse_analytics_sql,
};

/// `llm_cost_daily` is the per-day spend summary the analytics API installs
/// beside `llm_calls` when a query names it: content-free sums only.
const ALLOWED_RELATIONS: &[&str] = &[
    "llm_calls",
    "chat_session_cache_summary",
    "llm_embeddings",
    "llm_cost_daily",
];
pub const LEGACY_ERROR_REDACTION: &str = "legacy_error_redacted";
pub const LEGACY_INVALID_CATEGORY_REDACTION: &str = "invalid_category_redacted";

struct CompatibilityColumn {
    name: &'static str,
    ty: &'static str,
    default_sql: String,
}

/// Parse and authorize one compatibility query.
///
/// A lexical token scan is insufficient here: nested subqueries, CTE
/// shadowing, qualified catalog relations and table functions can all bypass
/// it. The AST visitor permits only the two projected compatibility views and
/// in-scope local CTEs derived from those views.
pub fn validate_legacy_llm_query(sql: &str) -> Result<()> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err(anyhow!("legacy LLM SQL must not be empty"));
    }
    let mut statements =
        parse_analytics_sql(sql).map_err(|error| anyhow!("invalid legacy LLM SQL: {error}"))?;
    if statements.len() != 1 {
        return Err(anyhow!("legacy LLM SQL requires exactly one statement"));
    }
    let statement = statements.pop().expect("one parsed statement");
    let Statement::Query(query) = &statement else {
        return Err(anyhow!("legacy LLM SQL accepts one SELECT/WITH query only"));
    };
    if !is_read_only_select_query(query) {
        return Err(anyhow!(
            "legacy LLM SQL permits side-effect-free SELECT query bodies only"
        ));
    }

    let mut visitor = LegacyLlmSqlVisitor::default();
    if let ControlFlow::Break(error) = statement.visit(&mut visitor) {
        return Err(anyhow!(error));
    }
    if !visitor.saw_allowed_relation {
        return Err(anyhow!(
            "legacy LLM SQL must reference an allowlisted compatibility relation"
        ));
    }
    Ok(())
}

#[derive(Default)]
struct LegacyLlmSqlVisitor {
    cte_scopes: Vec<HashSet<String>>,
    saw_allowed_relation: bool,
}

impl LegacyLlmSqlVisitor {
    fn cte_is_in_scope(&self, relation: &str) -> bool {
        self.cte_scopes
            .iter()
            .rev()
            .any(|scope| scope.contains(relation))
    }
}

impl Visitor for LegacyLlmSqlVisitor {
    type Break = String;

    fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> ControlFlow<Self::Break> {
        if !is_read_only_select_query_node(query) {
            return ControlFlow::Break(
                "legacy LLM SQL permits side-effect-free SELECT query bodies only".to_string(),
            );
        }
        let Some(with) = &query.with else {
            self.cte_scopes.push(HashSet::new());
            return ControlFlow::Continue(());
        };

        if !with.recursive {
            self.cte_scopes.push(HashSet::new());
            for cte in &with.cte_tables {
                if let ControlFlow::Break(error) = cte.visit(self) {
                    return ControlFlow::Break(error);
                }
                let name = cte.alias.name.value.to_ascii_lowercase();
                if ALLOWED_RELATIONS.contains(&name.as_str()) {
                    return ControlFlow::Break(format!(
                        "CTE `{name}` may not shadow a legacy LLM relation"
                    ));
                }
                if let Some(scope) = self.cte_scopes.last_mut() {
                    if !scope.insert(name.clone()) {
                        return ControlFlow::Break(format!(
                            "CTE `{name}` is defined more than once"
                        ));
                    }
                }
            }
            return ControlFlow::Continue(());
        }

        let mut scope = HashSet::new();
        for cte in &with.cte_tables {
            let name = cte.alias.name.value.to_ascii_lowercase();
            if ALLOWED_RELATIONS.contains(&name.as_str()) {
                return ControlFlow::Break(format!(
                    "CTE `{name}` may not shadow a legacy LLM relation"
                ));
            }
            if !scope.insert(name.clone()) {
                return ControlFlow::Break(format!("CTE `{name}` is defined more than once"));
            }
        }
        self.cte_scopes.push(scope);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &sqlparser::ast::Query) -> ControlFlow<Self::Break> {
        let _ = self.cte_scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(
        &mut self,
        relation: &sqlparser::ast::ObjectName,
    ) -> ControlFlow<Self::Break> {
        let relation = relation.to_string();
        if relation.contains('.') {
            return ControlFlow::Break(format!(
                "qualified or external relation `{relation}` is not available to legacy LLM SQL"
            ));
        }
        let normalized = relation.trim_matches('"').to_ascii_lowercase();
        if self.cte_is_in_scope(&normalized) {
            return ControlFlow::Continue(());
        }
        if !ALLOWED_RELATIONS.contains(&normalized.as_str()) {
            return ControlFlow::Break(format!(
                "relation `{relation}` is not an allowlisted legacy LLM view"
            ));
        }
        self.saw_allowed_relation = true;
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        match factor {
            TableFactor::Table { args: None, .. }
            | TableFactor::Derived { .. }
            | TableFactor::NestedJoin { .. } => ControlFlow::Continue(()),
            TableFactor::Table { args: Some(_), .. } => ControlFlow::Break(
                "table-valued arguments are not available to legacy LLM SQL".to_string(),
            ),
            _ => ControlFlow::Break(
                "table functions, UNNEST, PIVOT, and external scans are not available to legacy LLM SQL"
                    .to_string(),
            ),
        }
    }

    fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Self::Break> {
        let Expr::Function(function) = expression else {
            return ControlFlow::Continue(());
        };
        let function_name = function.name.to_string().to_ascii_lowercase();
        let unqualified = function_name
            .rsplit('.')
            .next()
            .unwrap_or(function_name.as_str())
            .trim_matches('"');
        let forbidden = [
            "getenv",
            "glob",
            "http_get",
            "http_post",
            "load_extension",
            "read_blob",
            "read_csv",
            "read_csv_auto",
            "read_json",
            "read_json_auto",
            "read_ndjson",
            "read_parquet",
            "read_text",
            "sqlite_scan",
            "postgres_scan",
        ];
        if forbidden.contains(&unqualified) || unqualified.starts_with("read_") {
            return ControlFlow::Break(format!(
                "function `{}` is not available to legacy LLM SQL",
                function.name
            ));
        }
        ControlFlow::Continue(())
    }
}

/// Install stable, content-free compatibility relations over server-selected files.
///
/// The raw Parquet scan is never installed as a named relation. Historical
/// `reasoning_summary` is omitted, while the legacy free-form `error` field is
/// reduced to a fixed presence marker.
///
/// `llm_calls` is a lazily evaluated VIEW, not a materialized table. It used
/// to be `CREATE TEMP TABLE … AS SELECT` so it would stay readable after the
/// caller disables external access, but that copied every call in the
/// selected files (and, through a materialized `SELECT *` CTE, every raw
/// column) on each request: an unbounded `llm_calls` query over a month of
/// history ran out of the 512 MB + 1 GB spill budget before user SQL started.
/// The view streams instead, so the caller's filters, projection and LIMIT
/// push down into the scan. It stays readable after external access is
/// disabled because exactly these files are registered in DuckDB's
/// `allowed_paths` (see [`allow_server_selected_parquet`]); every other file
/// and every directory remains unreachable.
///
/// `read_source_sql` / `attempts_source_sql` must name explicit files (one
/// quoted path or a `[...]` list of them), not glob patterns: `allowed_paths`
/// matches files, so a glob would fail closed once external access is off.
pub fn install_legacy_llm_views(
    conn: &Connection,
    read_source_sql: &str,
    attempts_source_sql: Option<&str>,
    principal: &str,
    workspace: &str,
    // When every query on this connection has a numeric `timestamp_ms >= N`
    // window, the bound is written into the raw Parquet scan itself so the
    // reconciliation joins only see the window's rows. `None` reads every
    // selected file (still streamed, never materialized).
    timestamp_lower_bound_ms: Option<i64>,
) -> Result<()> {
    install_legacy_llm_views_inner(
        conn,
        read_source_sql,
        attempts_source_sql,
        principal,
        workspace,
        timestamp_lower_bound_ms,
        false,
    )
}

/// Daily rollups scan only a three-partition window. Materialize its narrow
/// compatibility columns once so reconciliation does not repeatedly bind every
/// Parquet file. General queries retain streaming views and their memory cap.
pub fn install_legacy_llm_views_for_daily_summary(
    conn: &Connection,
    read_source_sql: &str,
    attempts_source_sql: Option<&str>,
    principal: &str,
    workspace: &str,
) -> Result<()> {
    install_legacy_llm_views_inner(
        conn,
        read_source_sql,
        attempts_source_sql,
        principal,
        workspace,
        None,
        true,
    )
}

/// Materialize a caller-bounded recent window's narrow source once before
/// reconciliation. The bound is applied before deduplication, exactly as in
/// the streaming compatibility view; provider attempts keep their full join scope.
pub fn install_legacy_llm_views_for_recent_window(
    conn: &Connection,
    read_source_sql: &str,
    attempts_source_sql: Option<&str>,
    principal: &str,
    workspace: &str,
    timestamp_lower_bound_ms: i64,
) -> Result<()> {
    install_legacy_llm_views_inner(
        conn,
        read_source_sql,
        attempts_source_sql,
        principal,
        workspace,
        Some(timestamp_lower_bound_ms),
        true,
    )
}

fn install_legacy_llm_views_inner(
    conn: &Connection,
    read_source_sql: &str,
    attempts_source_sql: Option<&str>,
    principal: &str,
    workspace: &str,
    timestamp_lower_bound_ms: Option<i64>,
    materialize_daily: bool,
) -> Result<()> {
    let columns = parquet_source_columns(conn, read_source_sql)?;
    let timestamp_default = if columns.contains("started_at_ms") {
        "CAST(started_at_ms AS BIGINT)".to_string()
    } else {
        "0::BIGINT".to_string()
    };
    let started_at_default = if columns.contains("timestamp_ms") {
        "CAST(timestamp_ms AS BIGINT)".to_string()
    } else {
        "0::BIGINT".to_string()
    };
    let principal_default = format!("'{}'::VARCHAR", escape_sql_literal(principal));
    let workspace_default = format!("'{}'::VARCHAR", escape_sql_literal(workspace));
    let column_defs = vec![
        CompatibilityColumn {
            name: "timestamp_ms",
            ty: "BIGINT",
            default_sql: timestamp_default,
        },
        CompatibilityColumn {
            name: "started_at_ms",
            ty: "BIGINT",
            default_sql: started_at_default,
        },
        CompatibilityColumn {
            name: "latency_ms",
            ty: "BIGINT",
            default_sql: "NULL::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "principal",
            ty: "VARCHAR",
            default_sql: principal_default,
        },
        CompatibilityColumn {
            name: "workspace",
            ty: "VARCHAR",
            default_sql: workspace_default,
        },
        CompatibilityColumn {
            name: "schema_version",
            ty: "INTEGER",
            default_sql: "0::INTEGER".into(),
        },
        CompatibilityColumn {
            name: "trace_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "llm_call_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "provider_attempt_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "dispatch_job_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "parent_call_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "parent_relation",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "retry_group_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "route_decision_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "scope_resolution",
            ty: "VARCHAR",
            default_sql: "'legacy_default'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "root_execution_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "iteration_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "prompt_projection_mode",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "chat_turn_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "workload_class",
            ty: "VARCHAR",
            default_sql: "'system'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "call_role",
            ty: "VARCHAR",
            default_sql: "'primary'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "provider_attempt_count",
            ty: "INTEGER",
            default_sql: "0::INTEGER".into(),
        },
        CompatibilityColumn {
            name: "response_reused",
            ty: "BOOLEAN",
            default_sql: "FALSE::BOOLEAN".into(),
        },
        CompatibilityColumn {
            name: "execution_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "task_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "plan_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "step_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "step_index",
            ty: "BIGINT",
            default_sql: "NULL::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "agent_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "delegated_agent_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "chat_session_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "operation",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "profile",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "provider",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "model",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "capability",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "response_kind",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "attempt",
            ty: "INTEGER",
            default_sql: "1::INTEGER".into(),
        },
        CompatibilityColumn {
            name: "success",
            ty: "BOOLEAN",
            default_sql: "TRUE::BOOLEAN".into(),
        },
        CompatibilityColumn {
            name: "error",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "input_tokens",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "output_tokens",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "reasoning_tokens",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "cache_read_tokens",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "cache_creation_tokens",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "ttft_ms",
            ty: "BIGINT",
            default_sql: "NULL::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "cost_usd",
            ty: "DOUBLE",
            default_sql: "0.0::DOUBLE".into(),
        },
        CompatibilityColumn {
            name: "dt",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
    ];
    let projection = column_defs
        .iter()
        .map(|column| compatibility_projection_expr(&columns, column))
        .collect::<Vec<_>>()
        .join(",\n                  ");
    let source_columns = compatibility_source_columns_sql(&columns, &column_defs);
    let materialized = if materialize_daily {
        let window_filter = match timestamp_lower_bound_ms {
            Some(bound) if columns.contains("timestamp_ms") => {
                format!(" WHERE timestamp_ms >= {bound}")
            },
            _ => String::new(),
        };
        conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE _legacy_llm_daily_raw AS SELECT {source_columns} FROM read_parquet({read_source_sql}, hive_partitioning = true, union_by_name = true){window_filter}"
        )).map_err(|error| anyhow!("prepare narrow daily LLM source: {error}"))?;
        Some("_legacy_llm_daily_raw")
    } else {
        None
    };
    if materialize_daily && columns.contains("record_kind") {
        if let Some(attempts) = attempts_source_sql {
            // View binding also expands the winning-attempt CTE repeatedly.
            // Keep its five required fields in memory beside the narrow calls
            // window; provider payloads and unrelated fields never enter it.
            conn.execute_batch(&format!(
                "CREATE OR REPLACE TEMP TABLE _legacy_llm_daily_attempts AS \
                 SELECT llm_call_id, provider, model, attempt_terminal_state, provider_attempt_index \
                 FROM read_parquet({attempts}, hive_partitioning = true, union_by_name = true)"
            )).map_err(|error| anyhow!("prepare narrow daily LLM attempts: {error}"))?;
        }
    }
    let calls_relation = build_llm_calls_relation(
        read_source_sql,
        attempts_source_sql,
        &columns,
        &source_columns,
        timestamp_lower_bound_ms,
        materialized,
    );
    allow_server_selected_parquet(conn, read_source_sql)?;
    if let Some(attempts) = attempts_source_sql {
        allow_server_selected_parquet(conn, attempts)?;
    }
    let view_sql = format!(
        "CREATE OR REPLACE VIEW llm_calls AS
           SELECT {projection}
           FROM {calls_relation};
         CREATE OR REPLACE VIEW chat_session_cache_summary AS
           SELECT chat_session_id,
                  COUNT(*) AS turns,
                  SUM(input_tokens) AS total_input,
                  SUM(cache_read_tokens) AS total_cached,
                  SUM(cache_creation_tokens) AS total_cache_writes,
                  ROUND(100.0 * SUM(cache_read_tokens) / NULLIF(SUM(input_tokens), 0), 1) AS cache_hit_pct,
                  SUM(output_tokens) AS total_output,
                  SUM(reasoning_tokens) AS total_reasoning,
                  ROUND(AVG(ttft_ms), 0) AS avg_ttft_ms,
                  ROUND(AVG(latency_ms), 0) AS avg_latency_ms,
                  SUM(cost_usd) AS total_cost_usd,
                  MIN(timestamp_ms) AS first_call_ms,
                  MAX(timestamp_ms) AS last_call_ms
           FROM llm_calls
           WHERE chat_session_id IS NOT NULL
             AND success = true
           GROUP BY chat_session_id;"
    );
    conn.execute_batch(&view_sql)
        .map_err(|error| anyhow!("create content-free legacy llm_calls views: {error}"))?;
    Ok(())
}

/// Register server-selected Parquet files in DuckDB's `allowed_paths`, merged
/// with any files an earlier install on this connection registered.
///
/// `allowed_paths` only matters once `enable_external_access = false`: those
/// exact files stay readable (so lazily bound compatibility views keep
/// working), while every other file, directory and table function stays
/// blocked. DuckDB refuses to change the list after external access is off, so
/// user SQL cannot widen it. Caller SQL is still AST-allowlisted
/// ([`validate_legacy_llm_query`]) and may not call `read_*` itself.
fn allow_server_selected_parquet(conn: &Connection, source_sql: &str) -> Result<()> {
    let paths = parquet_source_sql_list(source_sql);
    conn.execute_batch(&format!(
        "SET allowed_paths = list_distinct(list_concat(current_setting('allowed_paths'), {paths}));"
    ))
    .map_err(|error| anyhow!("register server-selected analytics Parquet files: {error}"))
}

/// The `read_parquet` argument forms callers pass (`'one/path'` or
/// `['a', 'b']`) as a SQL list literal.
fn parquet_source_sql_list(source_sql: &str) -> String {
    let source_sql = source_sql.trim();
    if source_sql.starts_with('[') {
        source_sql.to_string()
    } else {
        format!("[{source_sql}]")
    }
}

/// The physical source columns the compatibility projection and the
/// one-row-per-call reconciliation read, as a quoted select list.
///
/// Scanning only these (instead of `SELECT *`) keeps restricted and bulky raw
/// columns (`reasoning_summary`, checksums, idempotency keys, ~100 canonical
/// fact columns) out of every intermediate relation.
fn compatibility_source_columns_sql(
    columns: &HashSet<String>,
    column_defs: &[CompatibilityColumn],
) -> String {
    let mut needed = column_defs
        .iter()
        .map(|column| column.name)
        .chain(["record_kind", "record_revision"])
        .filter(|name| columns.contains(*name))
        .collect::<Vec<_>>();
    needed.sort_unstable();
    needed.dedup();
    if needed.is_empty() {
        // Every compatibility column is defaulted; the scan only supplies rows.
        return "TRUE AS __compat_row".to_string();
    }
    needed
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Cap an allowlisted compatibility query at `max_rows` result rows when it
/// has no row limit of its own.
///
/// The handlers return at most `max_rows` rows anyway, but DuckDB's Rust
/// `query` materializes the whole result first, so an unbounded
/// `SELECT * FROM llm_calls` would build the full result set inside the
/// memory budget only to discard all but the first page. Appending the LIMIT
/// to the top-level query keeps its ORDER BY semantics and lets DuckDB stop
/// early. Queries that already carry LIMIT/FETCH, or that do not parse as one
/// query (they are rejected elsewhere), are returned unchanged.
pub fn bound_legacy_llm_query_rows(sql: &str, max_rows: usize) -> String {
    let trimmed = sql
        .trim()
        .trim_end_matches(|ch: char| ch == ';' || ch.is_whitespace());
    let Ok(statements) = parse_analytics_sql(trimmed) else {
        return sql.to_string();
    };
    let [Statement::Query(query)] = statements.as_slice() else {
        return sql.to_string();
    };
    if query.limit_clause.is_some() || query.fetch.is_some() {
        return sql.to_string();
    }
    // A newline (not a space) so a trailing `-- comment` cannot swallow it.
    format!("{trimmed}\nLIMIT {max_rows}")
}

/// Slack taken off a bound computed from `now()`/`current_date`. DuckDB
/// evaluates those in the session time zone (`epoch_ms(CAST(now() AS
/// TIMESTAMP))` is local wall time read back as UTC, up to 14 h either way)
/// and `current_date` truncates to midnight, so two days keeps the pushed
/// bound at or below every value the query itself compares against.
pub const LLM_CALLS_RELATIVE_BOUND_SLACK_MS: i64 = 2 * 24 * 60 * 60 * 1000;

/// The lowest `timestamp_ms` any scan of `llm_calls` in `sql` can need, in
/// epoch milliseconds, or `None` when that cannot be proven.
///
/// The bound is pushed into the shared reconciled relation, which is only
/// correct when every SELECT reading `llm_calls` filters by one, so this
/// walks the AST rather than the text: a `CASE WHEN timestamp_ms >= …` or an
/// `OR` arm is not a bound. A SELECT is bounded when `llm_calls` is its only
/// FROM item and a top-level `AND` conjunct of its WHERE is
/// `timestamp_ms >|>= <b>` (or `BETWEEN <b> AND …`), where `<b>` is an integer
/// literal or `epoch_ms(now()|current_timestamp|current_date|today()
/// [- INTERVAL n second|minute|hour|day|week])`. Relative bounds use `now_ms`
/// minus [`LLM_CALLS_RELATIVE_BOUND_SLACK_MS`]; DuckDB reads its clock later,
/// which only raises the query's own threshold. Joins, a
/// `chat_session_cache_summary` read, column-renaming aliases, unknown
/// expressions and parse failures all yield `None` (read unbounded).
pub fn llm_calls_timestamp_lower_bound_ms(sql: &str, now_ms: i64) -> Option<i64> {
    let statements = parse_analytics_sql(sql).ok()?;
    let [statement @ Statement::Query(_)] = statements.as_slice() else {
        return None;
    };
    let mut visitor = LlmCallsBoundVisitor {
        now_ms,
        ..LlmCallsBoundVisitor::default()
    };
    let _ = statement.visit(&mut visitor);
    // Every `llm_calls` reference must be a FROM item this walk judged; one
    // seen anywhere else (a nested join, `TABLE llm_calls`) is unbounded.
    if visitor.unbounded || visitor.scans == 0 || visitor.scans != visitor.relation_refs {
        return None;
    }
    visitor.bound
}

#[derive(Default)]
struct LlmCallsBoundVisitor {
    now_ms: i64,
    relation_refs: usize,
    scans: usize,
    bound: Option<i64>,
    unbounded: bool,
}

impl LlmCallsBoundVisitor {
    fn visit_set_expr(&mut self, body: &SetExpr) {
        match body {
            SetExpr::Select(select) => self.visit_select(select),
            SetExpr::SetOperation { left, right, .. } => {
                self.visit_set_expr(left);
                self.visit_set_expr(right);
            },
            // A parenthesised query is entered through `pre_visit_query`.
            _ => {},
        }
    }

    fn visit_select(&mut self, select: &Select) {
        let factors: Vec<&TableFactor> = select
            .from
            .iter()
            .flat_map(|from| {
                std::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation))
            })
            .collect();
        let mut qualifier = None;
        for factor in &factors {
            if let TableFactor::Table {
                name,
                alias,
                args: None,
                ..
            } = factor
            {
                if relation_name(name) != "llm_calls" {
                    continue;
                }
                self.scans += 1;
                match alias {
                    // `AS t(a, b)` renames columns, so `timestamp_ms` may no
                    // longer be the view's column.
                    Some(alias) if !alias.columns.is_empty() => self.unbounded = true,
                    Some(alias) => qualifier = Some(alias.name.value.clone()),
                    None => qualifier = Some("llm_calls".to_string()),
                }
            }
        }
        let Some(qualifier) = qualifier else {
            return;
        };
        let bound = (factors.len() == 1)
            .then_some(select.selection.as_ref())
            .flatten()
            .and_then(|selection| conjunct_lower_bound(selection, &qualifier, self.now_ms));
        match bound {
            Some(bound) => {
                self.bound = Some(self.bound.map_or(bound, |current| current.min(bound)));
            },
            None => self.unbounded = true,
        }
    }
}

impl Visitor for LlmCallsBoundVisitor {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        self.visit_set_expr(&query.body);
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        match relation_name(relation).as_str() {
            "llm_calls" => self.relation_refs += 1,
            // A per-session rollup over all of `llm_calls`.
            "chat_session_cache_summary" => self.unbounded = true,
            _ => {},
        }
        ControlFlow::Continue(())
    }
}

/// Normalised exactly as [`LegacyLlmSqlVisitor::pre_visit_relation`] does.
fn relation_name(relation: &ObjectName) -> String {
    relation.to_string().trim_matches('"').to_ascii_lowercase()
}

/// The tightest lower bound among the top-level `AND` conjuncts of `expr`.
fn conjunct_lower_bound(expr: &Expr, qualifier: &str, now_ms: i64) -> Option<i64> {
    match expr {
        Expr::Nested(inner) => conjunct_lower_bound(inner, qualifier, now_ms),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => match (
            conjunct_lower_bound(left, qualifier, now_ms),
            conjunct_lower_bound(right, qualifier, now_ms),
        ) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        },
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Gt | BinaryOperator::GtEq,
            right,
        } if is_timestamp_ms_column(left, qualifier) => bound_value_ms(right, now_ms),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Lt | BinaryOperator::LtEq,
            right,
        } if is_timestamp_ms_column(right, qualifier) => bound_value_ms(left, now_ms),
        Expr::Between {
            expr,
            negated: false,
            low,
            ..
        } if is_timestamp_ms_column(expr, qualifier) => bound_value_ms(low, now_ms),
        _ => None,
    }
}

fn is_timestamp_ms_column(expr: &Expr, qualifier: &str) -> bool {
    match expr {
        Expr::Nested(inner) => is_timestamp_ms_column(inner, qualifier),
        Expr::Identifier(column) => column.value.eq_ignore_ascii_case("timestamp_ms"),
        Expr::CompoundIdentifier(parts) => matches!(
            parts.as_slice(),
            [table, column]
                if table.value.eq_ignore_ascii_case(qualifier)
                    && column.value.eq_ignore_ascii_case("timestamp_ms")
        ),
        _ => false,
    }
}

/// An integer literal, or `epoch_ms(<relative instant>)` less the slack.
fn bound_value_ms(expr: &Expr, now_ms: i64) -> Option<i64> {
    match expr {
        Expr::Nested(inner) => bound_value_ms(inner, now_ms),
        Expr::Value(value) => match &value.value {
            Value::Number(number, _) => number.parse::<i64>().ok(),
            _ => None,
        },
        Expr::Function(function) if function_name(&function.name) == "epoch_ms" => {
            let arguments = plain_function_arguments(function)?;
            let [instant] = arguments.as_slice() else {
                return None;
            };
            relative_instant_ms(instant, now_ms)?.checked_sub(LLM_CALLS_RELATIVE_BOUND_SLACK_MS)
        },
        _ => None,
    }
}

/// `now()`-like clock reads, optionally cast to a timestamp/date type, minus
/// fixed-length intervals.
fn relative_instant_ms(expr: &Expr, now_ms: i64) -> Option<i64> {
    match expr {
        Expr::Nested(inner) => relative_instant_ms(inner, now_ms),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Minus,
            right,
        } => {
            let Expr::Interval(interval) = right.as_ref() else {
                return None;
            };
            relative_instant_ms(left, now_ms)?.checked_sub(interval_ms(interval)?)
        },
        Expr::Cast {
            expr, data_type, ..
        } if is_clock_type(data_type) => relative_instant_ms(expr, now_ms),
        Expr::Function(function) => {
            if !plain_function_arguments(function)?.is_empty() {
                return None;
            }
            matches!(
                function_name(&function.name).as_str(),
                "now"
                    | "current_timestamp"
                    | "get_current_timestamp"
                    | "transaction_timestamp"
                    | "current_date"
                    | "today"
            )
            .then_some(now_ms)
        },
        _ => None,
    }
}

fn is_clock_type(data_type: &DataType) -> bool {
    let name = data_type.to_string().to_ascii_uppercase();
    name.starts_with("TIMESTAMP") || name == "DATE" || name == "DATETIME"
}

/// Fixed-length intervals only: months and years vary, so they are not bounds.
fn interval_ms(interval: &Interval) -> Option<i64> {
    if interval.last_field.is_some()
        || interval.leading_precision.is_some()
        || interval.fractional_seconds_precision.is_some()
    {
        return None;
    }
    let Expr::Value(value) = interval.value.as_ref() else {
        return None;
    };
    let (amount, unit) = match (&value.value, &interval.leading_field) {
        (Value::Number(amount, _), Some(field)) => (amount.clone(), field.to_string()),
        (Value::SingleQuotedString(amount), Some(field)) => {
            (amount.trim().to_string(), field.to_string())
        },
        (Value::SingleQuotedString(text), None) => {
            let mut parts = text.split_whitespace();
            let (Some(amount), Some(unit), None) = (parts.next(), parts.next(), parts.next())
            else {
                return None;
            };
            (amount.to_string(), unit.to_string())
        },
        _ => return None,
    };
    let amount = amount.parse::<i64>().ok().filter(|amount| *amount >= 0)?;
    let unit_ms: i64 = match unit.to_ascii_lowercase().as_str() {
        "second" | "seconds" => 1_000,
        "minute" | "minutes" => 60_000,
        "hour" | "hours" => 3_600_000,
        "day" | "days" => 86_400_000,
        "week" | "weeks" => 604_800_000,
        _ => return None,
    };
    amount.checked_mul(unit_ms)
}

fn function_name(name: &ObjectName) -> String {
    name.to_string().trim_matches('"').to_ascii_lowercase()
}

/// Positional expression arguments of a plain call (no `FILTER`, `OVER`,
/// `DISTINCT` or clauses); `CURRENT_TIMESTAMP` without parentheses has none.
fn plain_function_arguments(function: &Function) -> Option<Vec<&Expr>> {
    if function.filter.is_some() || function.over.is_some() {
        return None;
    }
    match &function.args {
        FunctionArguments::None => Some(Vec::new()),
        FunctionArguments::List(list)
            if list.duplicate_treatment.is_none() && list.clauses.is_empty() =>
        {
            list.args
                .iter()
                .map(|argument| match argument {
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
                    _ => None,
                })
                .collect()
        },
        _ => None,
    }
}

/// Install a flat, content-free `llm_embeddings` relation over server-selected
/// Parquet files.
///
/// Unlike `llm_calls`, the embeddings dataset is already flat (one row per embed
/// batch) — there are no lifecycle/attempt records to reconcile. This builds a
/// simple typed projection with `COALESCE` defaults for schema evolution: a
/// physically-absent legacy column is materialized as its default rather than
/// failing the query. `read_parquet` is never installed as a named relation, so
/// the allowlist boundary (only `llm_embeddings`) still holds for client SQL.
pub fn install_llm_embeddings_view(conn: &Connection, read_source_sql: &str) -> Result<()> {
    let columns = parquet_source_columns(conn, read_source_sql)?;
    let column_defs = vec![
        CompatibilityColumn {
            name: "timestamp_ms",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "principal",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "workspace",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "provider",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "model",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "operation",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".into(),
        },
        CompatibilityColumn {
            name: "input_tokens",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "batch_size",
            ty: "INTEGER",
            default_sql: "0::INTEGER".into(),
        },
        CompatibilityColumn {
            name: "cost_usd",
            ty: "DOUBLE",
            default_sql: "0.0::DOUBLE".into(),
        },
        CompatibilityColumn {
            name: "latency_ms",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        },
        CompatibilityColumn {
            name: "success",
            ty: "BOOLEAN",
            default_sql: "TRUE::BOOLEAN".into(),
        },
        CompatibilityColumn {
            name: "dt",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".into(),
        },
    ];
    let projection = column_defs
        .iter()
        .map(|column| embeddings_projection_expr(&columns, column))
        .collect::<Vec<_>>()
        .join(",\n                  ");
    // A streaming view over registered files, like `llm_calls`: the dataset
    // grows without bound, so it is never copied into a per-request table.
    allow_server_selected_parquet(conn, read_source_sql)?;
    let view_sql = format!(
        "CREATE OR REPLACE VIEW llm_embeddings AS
           SELECT {projection}
           FROM read_parquet({read_source_sql}, hive_partitioning = true, union_by_name = true);"
    );
    conn.execute_batch(&view_sql)
        .map_err(|error| anyhow!("create content-free llm_embeddings view: {error}"))?;
    Ok(())
}

/// Flat COALESCE projection for the embeddings view. A present column is cast to
/// its declared type with the default filling observed NULLs; an absent column
/// is materialized as its default. Embeddings carry no restricted content, so —
/// unlike `llm_calls` — there is nothing to redact here.
fn embeddings_projection_expr(columns: &HashSet<String>, column: &CompatibilityColumn) -> String {
    if columns.contains(column.name) {
        format!(
            "COALESCE(CAST({name} AS {ty}), {default_sql}) AS {name}",
            name = column.name,
            ty = column.ty,
            default_sql = column.default_sql.as_str()
        )
    } else {
        format!("{} AS {}", column.default_sql, column.name)
    }
}

/// Build the FROM relation for the compatibility `llm_calls` view: exactly one
/// row per call.
///
/// The governed-observability pipeline writes call-level `call_fact` lifecycle
/// rows (record_revision 1 `started`, 2 `completed`) into the same `llm_calls`
/// dataset as the legacy flat `batch_*` fact rows. Those `call_fact` rows carry
/// NULL provider/model by design (provider/model live on the per-attempt
/// records), so globbing them raw made the pulse `GROUP BY provider, model`
/// surface a dominant empty group that rendered as "unknown". This projection
/// reconciles to one row per call:
/// - prefer the legacy `batch_*` row (already flat, with provider/model/cost);
/// - otherwise the completed `call_fact` (record_revision = 2), with
///   provider/model reconstructed from its winning provider attempt (succeeded
///   first, else the last attempt) via `attempts_source_sql`.
///
/// A pure legacy lake (no `call_fact` records → no `record_kind` column) needs
/// no reconciliation and is read directly.
///
/// Every step is `NOT MATERIALIZED` and the raw scan selects only
/// `source_columns`. DuckDB otherwise materializes a CTE referenced more than
/// once, and `raw` (read by `batch` and `completed`) was a `SELECT *` over
/// ~150 raw columns: on one month of history that buffer alone exhausted the
/// analytics memory and spill budget. Inlined, each consumer streams its own
/// narrow, filter-pushed scan of the same server-selected files.
fn build_llm_calls_relation(
    read_source_sql: &str,
    attempts_source_sql: Option<&str>,
    columns: &HashSet<String>,
    source_columns: &str,
    timestamp_lower_bound_ms: Option<i64>,
    materialized: Option<&str>,
) -> String {
    // Push the window's lower bound into the scan only when the source actually
    // carries a `timestamp_ms` column (governed lakes do); otherwise leaving it
    // off preserves prior behaviour and avoids referencing a missing column.
    let window_filter = match timestamp_lower_bound_ms {
        Some(bound) if columns.contains("timestamp_ms") => {
            format!(" WHERE timestamp_ms >= {bound}")
        },
        _ => String::new(),
    };
    let raw = materialized.map(str::to_owned).unwrap_or_else(|| {
        format!("read_parquet({read_source_sql}, hive_partitioning = true, union_by_name = true)")
    });
    if !columns.contains("record_kind") {
        // Pure legacy lake: no reconciliation, just the narrow scan.
        return format!("(SELECT {source_columns} FROM {raw}{window_filter})");
    }
    let win_att = match attempts_source_sql {
        Some(attempts) => {
            let attempts = if materialized.is_some() {
                "_legacy_llm_daily_attempts".to_owned()
            } else {
                format!("read_parquet({attempts}, hive_partitioning = true, union_by_name = true)")
            };
            format!(
                "win_att AS NOT MATERIALIZED (
                 SELECT llm_call_id, provider, model FROM (
                     SELECT llm_call_id, provider, model,
                            ROW_NUMBER() OVER (
                                PARTITION BY llm_call_id
                                ORDER BY (attempt_terminal_state = 'succeeded') DESC,
                                         provider_attempt_index DESC NULLS LAST
                            ) AS rn
                     FROM {attempts}
                     WHERE COALESCE(NULLIF(model, ''), '') <> ''
                 ) WHERE rn = 1
             )"
            )
        },
        None => "win_att AS NOT MATERIALIZED (SELECT NULL::VARCHAR AS llm_call_id, \
                 NULL::VARCHAR AS provider, NULL::VARCHAR AS model WHERE false)"
            .to_string(),
    };
    format!(
        "(
           WITH raw AS NOT MATERIALIZED (SELECT {source_columns} FROM {raw}{window_filter}),
                batch AS NOT MATERIALIZED (SELECT * FROM raw WHERE record_kind IS NULL),
                completed AS NOT MATERIALIZED (
                    SELECT * FROM raw
                    WHERE record_kind = 'call_fact' AND record_revision = 2
                      AND llm_call_id NOT IN (
                          SELECT llm_call_id FROM batch WHERE llm_call_id IS NOT NULL
                      )
                ),
                {win_att},
                projected AS NOT MATERIALIZED (
                    SELECT c.* REPLACE(
                        COALESCE(NULLIF(c.provider, ''), w.provider) AS provider,
                        COALESCE(NULLIF(c.model, ''), w.model) AS model
                    )
                    FROM completed c LEFT JOIN win_att w USING (llm_call_id)
                )
           SELECT * FROM batch
           UNION ALL BY NAME
           SELECT * FROM projected
         )"
    )
}

fn parquet_source_columns(conn: &Connection, read_source_sql: &str) -> Result<HashSet<String>> {
    let mut statement = conn
        .prepare(&format!(
            "DESCRIBE SELECT * FROM read_parquet({read_source_sql}, hive_partitioning = true, union_by_name = true)"
        ))
        .map_err(|error| anyhow!("describe legacy llm_calls source: {error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| anyhow!("read legacy llm_calls source columns: {error}"))?;
    let mut columns = HashSet::new();
    for row in rows {
        columns
            .insert(row.map_err(|error| anyhow!("read legacy llm_calls source column: {error}"))?);
    }
    Ok(columns)
}

fn compatibility_projection_expr(
    columns: &HashSet<String>,
    column: &CompatibilityColumn,
) -> String {
    if column.name == "error" && columns.contains(column.name) {
        return format!(
            "CASE WHEN error IS NULL THEN NULL ELSE '{LEGACY_ERROR_REDACTION}'::VARCHAR END AS error"
        );
    }
    if column.name == "response_kind" && columns.contains(column.name) {
        return format!(
            "CASE WHEN response_kind IS NULL THEN NULL \
             WHEN regexp_full_match(CAST(response_kind AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}') \
             THEN CAST(response_kind AS VARCHAR) \
             ELSE '{LEGACY_INVALID_CATEGORY_REDACTION}'::VARCHAR END AS response_kind"
        );
    }
    if column.name == "cost_usd" && columns.contains(column.name) {
        // Non-finite cost becomes SQL NULL — the honest encoding of "unknown",
        // and the one every consumer already handles.
        //
        // `voice_orchestrator` wrote `f64::NAN` to mean "usage was not
        // reported" (fixed at source in v0.6.1086, but rows already on disk
        // keep it forever, and they cannot be repriced because the usage that
        // would price them was never captured). NaN propagates through `SUM`,
        // is NOT caught by `COALESCE(SUM(...), 0)` — NaN is not NULL — and
        // serialises to JSON `null`, so a single poisoned row silently blanks
        // an entire aggregate. Two such rows blanked the `/llm` 7-day spend
        // KPI, hiding $20.73 of real spend across 8,295 calls.
        //
        // Sanitising HERE rather than in each query is deliberate: `cost_usd`
        // is summed from ~10 call sites across `/llm`, Today's pulse, the
        // VibeDev rollup and channel stats, and every one of them would
        // otherwise need the same guard and would silently regress without it.
        // NULL keeps "unknown" distinguishable from a genuine $0 — the row's
        // `usage_reported` flag remains the signal for why.
        return format!(
            "CASE WHEN isfinite(CAST({name} AS {ty})) THEN CAST({name} AS {ty}) \
             ELSE NULL::{ty} END AS {name}",
            name = column.name,
            ty = column.ty
        );
    }
    if columns.contains(column.name) {
        // Defaults are a schema-evolution device for physically absent legacy
        // columns, not permission to rewrite an observed SQL NULL. Preserving
        // unknown cost/token/route values prevents compatibility analytics
        // from inventing zero spend or an "unknown" provider observation.
        format!(
            "CAST({name} AS {ty}) AS {name}",
            name = column.name,
            ty = column.ty
        )
    } else {
        format!("{} AS {}", column.default_sql, column.name)
    }
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn daily_usage_materialization_preserves_reconciliation_and_redaction() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("calls.parquet");
        let attempts = tmp.path().join("attempts.parquet");
        let writer = Connection::open_in_memory().unwrap();
        writer.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES
             ('a', NULL::VARCHAR, NULL::BIGINT, 'batch', 'provider', 1.0::DOUBLE, 1700000000000::BIGINT, 'private reasoning', 'private error'),
             ('a', 'call_fact', 2, 'duplicate', 'provider', 2.0, 1700000000000, 'private reasoning', 'private error'),
             ('b', 'call_fact', 2, '', '', 3.0, 1700000000001, 'private reasoning', 'private error')
             ) t(llm_call_id, record_kind, record_revision, model, provider, cost_usd, timestamp_ms, reasoning_summary, error)) TO '{}' (FORMAT PARQUET)", escape_sql_literal(&file.to_string_lossy())
        )).unwrap();
        writer.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES \
             ('b', 'provider', 'canonical', 'succeeded', 1, 'private provider payload'), \
             ('b', 'provider', 'failed-model', 'failed', 2, 'private provider payload') \
             ) t(llm_call_id, provider, model, attempt_terminal_state, provider_attempt_index, response_payload)) \
             TO '{}' (FORMAT PARQUET)", escape_sql_literal(&attempts.to_string_lossy())
        )).unwrap();
        let source = format!("'{}'", escape_sql_literal(&file.to_string_lossy()));
        let attempts_source = format!("'{}'", escape_sql_literal(&attempts.to_string_lossy()));
        for materialize in [false, true] {
            let conn = Connection::open_in_memory().unwrap();
            if materialize {
                install_legacy_llm_views_for_daily_summary(
                    &conn,
                    &source,
                    Some(&attempts_source),
                    "owner",
                    "workspace",
                )
                .unwrap();
                let cols: Vec<String> = conn
                    .prepare("DESCRIBE _legacy_llm_daily_raw")
                    .unwrap()
                    .query_map([], |r| r.get(0))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                assert!(!cols.iter().any(|c| c == "reasoning_summary"));
                let attempt_columns: Vec<String> = conn
                    .prepare("DESCRIBE _legacy_llm_daily_attempts")
                    .unwrap()
                    .query_map([], |r| r.get(0))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                assert_eq!(attempt_columns.len(), 5);
                assert!(!attempt_columns.iter().any(|c| c == "response_payload"));
                // Withdraw only these tempdir fixtures: materialized daily
                // views must no longer reopen either Parquet source.
                std::fs::remove_file(&file).unwrap();
                std::fs::remove_file(&attempts).unwrap();
            } else {
                install_legacy_llm_views(
                    &conn,
                    &source,
                    Some(&attempts_source),
                    "owner",
                    "workspace",
                    None,
                )
                .unwrap();
            }
            conn.execute_batch("SET enable_external_access = false")
                .unwrap();
            let (count, cost, errors): (i64, f64, String) = conn
                .query_row(
                    "SELECT COUNT(*), SUM(cost_usd), MIN(error) FROM llm_calls",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                (count, cost, errors.as_str()),
                (2, 4.0, LEGACY_ERROR_REDACTION)
            );
            let model: String = conn
                .query_row(
                    "SELECT model FROM llm_calls WHERE llm_call_id = 'b'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(model, "canonical");
        }
        assert!(validate_legacy_llm_query("SELECT * FROM _legacy_llm_daily_raw").is_err());
        assert!(validate_legacy_llm_query("SELECT * FROM _legacy_llm_daily_attempts").is_err());
    }

    /// A non-finite `cost_usd` must reach consumers as SQL NULL.
    ///
    /// Regression guard. `f64::NAN` survives Parquet, propagates through
    /// `SUM`, is NOT caught by `COALESCE(SUM(...), 0)`, and serialises to JSON
    /// `null` — so one poisoned row silently blanks a whole aggregate. Two of
    /// them blanked the `/llm` 7-day spend KPI over 8,295 calls. Sanitising in
    /// the projection is what stops every downstream `SUM(cost_usd)` from
    /// needing its own guard.
    #[test]
    fn cost_usd_projection_nulls_non_finite_values() {
        let mut columns = HashSet::new();
        columns.insert("cost_usd".to_string());
        let column = CompatibilityColumn {
            name: "cost_usd",
            ty: "DOUBLE",
            default_sql: "0.0::DOUBLE".into(),
        };
        let sql = compatibility_projection_expr(&columns, &column);
        assert!(
            sql.contains("isfinite"),
            "cost projection must screen NaN/Inf: {sql}"
        );
        assert!(
            sql.contains("NULL::DOUBLE"),
            "unknown cost must be NULL, not 0: {sql}"
        );
        assert!(
            sql.ends_with("AS cost_usd"),
            "must keep the column name: {sql}"
        );
    }

    /// Other numeric columns keep the plain cast — the NaN screen is specific
    /// to cost, which is the only column a provider gap writes NaN into.
    #[test]
    fn other_columns_keep_the_plain_cast() {
        let mut columns = HashSet::new();
        columns.insert("input_tokens".to_string());
        let column = CompatibilityColumn {
            name: "input_tokens",
            ty: "BIGINT",
            default_sql: "0::BIGINT".into(),
        };
        let sql = compatibility_projection_expr(&columns, &column);
        assert_eq!(sql, "CAST(input_tokens AS BIGINT) AS input_tokens");
    }

    #[test]
    fn compatibility_sql_is_ast_allowlisted() {
        for sql in [
            "SELECT operation, COUNT(*) FROM llm_calls GROUP BY operation",
            "WITH scoped AS (SELECT * FROM llm_calls) SELECT COUNT(*) FROM scoped",
            "SELECT * FROM chat_session_cache_summary",
            "SELECT provider, model, COUNT(*) FROM llm_embeddings GROUP BY provider, model",
            "WITH e AS (SELECT * FROM llm_embeddings) SELECT SUM(input_tokens) FROM e",
        ] {
            validate_legacy_llm_query(sql).expect("allowlisted query");
        }

        for sql in [
            "SELECT 1",
            "SELECT * FROM llm_calls_raw",
            "SELECT * FROM main.llm_calls",
            "SELECT * FROM duckdb_tables()",
            "SELECT * FROM read_parquet('/tmp/private.parquet')",
            "WITH llm_calls AS (SELECT 1) SELECT * FROM llm_calls",
            "WITH x AS (SELECT * FROM x) SELECT * FROM llm_calls",
            "SELECT * FROM llm_calls; SELECT * FROM llm_calls",
            "SELECT * INTO temporary_llm_copy FROM llm_calls",
            "WITH scoped AS (SELECT * FROM llm_calls) VALUES (1)",
            "WITH payload AS (VALUES (1)) SELECT * FROM llm_calls",
            "SELECT * FROM llm_calls WHERE EXISTS (SELECT * FROM (VALUES (1)) AS payload(value))",
            "TABLE llm_calls",
            // The embeddings relation is allowlisted, but adjacent/unknown
            // relation names must still be rejected.
            "SELECT * FROM llm_embeddings_raw",
            "SELECT * FROM main.llm_embeddings",
            "WITH llm_embeddings AS (SELECT 1) SELECT * FROM llm_embeddings",
        ] {
            assert!(
                validate_legacy_llm_query(sql).is_err(),
                "unsafe query was accepted: {sql}"
            );
        }
    }

    #[test]
    fn llm_embeddings_view_is_flat_and_content_free() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parquet = temp.path().join("embeddings.parquet");
        let writer = Connection::open_in_memory().expect("writer");
        // Two flat embed-batch rows, as the sink writes them.
        writer
            .execute_batch(&format!(
                "COPY (SELECT * FROM (VALUES
                     (1700000000000::BIGINT, 'p'::VARCHAR, 'w'::VARCHAR, 'ollama'::VARCHAR, 'embed-model'::VARCHAR, 'memory_index'::VARCHAR, 120::BIGINT, 3::INTEGER, 0.0::DOUBLE, 42::BIGINT, TRUE),
                     (1700000000001::BIGINT, 'p'::VARCHAR, 'w'::VARCHAR, 'ollama'::VARCHAR, 'embed-model'::VARCHAR, 'resurfacing'::VARCHAR, 400::BIGINT, 8::INTEGER, 0.0::DOUBLE, 55::BIGINT, TRUE)
                   ) AS t(timestamp_ms, principal, workspace, provider, model, operation, input_tokens, batch_size, cost_usd, latency_ms, success))
                 TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(&parquet.to_string_lossy())
            ))
            .expect("write embeddings fixture");

        let reader = Connection::open_in_memory().expect("reader");
        install_llm_embeddings_view(
            &reader,
            &format!("'{}'", escape_sql_literal(&parquet.to_string_lossy())),
        )
        .expect("install embeddings view");

        // Grouping surfaces the two flat rows with real provider/model.
        let (provider, model, rows): (String, String, i64) = reader
            .query_row(
                "SELECT provider, model, COUNT(*) FROM llm_embeddings GROUP BY provider, model",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("one provider/model group");
        assert_eq!(provider, "ollama");
        assert_eq!(model, "embed-model");
        assert_eq!(rows, 2);

        let (total, tokens): (i64, i64) = reader
            .query_row(
                "SELECT COUNT(*), SUM(input_tokens) FROM llm_embeddings",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("aggregate embeddings");
        assert_eq!(total, 2);
        assert_eq!(tokens, 520);

        // The raw parquet scan must never be reachable as a named relation.
        assert!(reader.prepare("SELECT * FROM llm_embeddings_raw").is_err());
    }

    #[test]
    fn llm_embeddings_view_defaults_absent_columns() {
        // A legacy/partial parquet missing later columns must still install:
        // absent columns materialize as their typed defaults.
        let temp = tempfile::tempdir().expect("tempdir");
        let parquet = temp.path().join("partial_embeddings.parquet");
        let writer = Connection::open_in_memory().expect("writer");
        writer
            .execute_batch(&format!(
                "COPY (SELECT 1700000000000::BIGINT AS timestamp_ms,
                              'ollama'::VARCHAR AS provider,
                              'embed-model'::VARCHAR AS model,
                              'elicitation'::VARCHAR AS operation)
                 TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(&parquet.to_string_lossy())
            ))
            .expect("write partial fixture");

        let reader = Connection::open_in_memory().expect("reader");
        install_llm_embeddings_view(
            &reader,
            &format!("'{}'", escape_sql_literal(&parquet.to_string_lossy())),
        )
        .expect("install embeddings view over partial parquet");

        let (batch_size, cost, success): (i32, f64, bool) = reader
            .query_row(
                "SELECT batch_size, cost_usd, success FROM llm_embeddings",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read defaulted columns");
        assert_eq!(batch_size, 0);
        assert_eq!(cost, 0.0);
        assert!(success);
    }

    #[test]
    fn compatibility_view_omits_reasoning_and_redacts_free_form_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parquet = temp.path().join("legacy.parquet");
        let writer = Connection::open_in_memory().expect("writer");
        writer
            .execute_batch(&format!(
                "COPY (SELECT 1700000000000::BIGINT AS timestamp_ms,
                              'chat'::VARCHAR AS operation,
                              'private chain of thought'::VARCHAR AS reasoning_summary,
                              'secret provider response'::VARCHAR AS error,
                              'private response text'::VARCHAR AS response_kind,
                              NULL::DOUBLE AS cost_usd,
                              NULL::BIGINT AS input_tokens,
                              NULL::VARCHAR AS provider)
                 TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(&parquet.to_string_lossy())
            ))
            .expect("write fixture");

        let reader = Connection::open_in_memory().expect("reader");
        install_legacy_llm_views(
            &reader,
            &format!("'{}'", escape_sql_literal(&parquet.to_string_lossy())),
            None,
            "owner",
            "default",
            None,
        )
        .expect("install compatibility views");

        let error: Option<String> = reader
            .query_row("SELECT error FROM llm_calls", [], |row| row.get(0))
            .expect("read redacted error");
        assert_eq!(error.as_deref(), Some(LEGACY_ERROR_REDACTION));
        let response_kind: String = reader
            .query_row("SELECT response_kind FROM llm_calls", [], |row| row.get(0))
            .expect("read redacted response category");
        assert_eq!(response_kind, LEGACY_INVALID_CATEGORY_REDACTION);
        let (cost, input_tokens, provider): (Option<f64>, Option<i64>, Option<String>) = reader
            .query_row(
                "SELECT cost_usd, input_tokens, provider FROM llm_calls",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read preserved unknowns");
        assert_eq!(cost, None);
        assert_eq!(input_tokens, None);
        assert_eq!(provider, None);
        assert!(reader
            .prepare("SELECT reasoning_summary FROM llm_calls")
            .is_err());
        assert!(reader.prepare("SELECT * FROM llm_calls_raw").is_err());
    }

    /// The governed observability pipeline writes call-level `call_fact`
    /// lifecycle rows (provider/model NULL by design) into the same `llm_calls`
    /// dataset the legacy compat lake globs. Those rows must NOT surface as
    /// their own provider/model group (which rendered as "unknown" and, being
    /// the most numerous, won the Today's Pulse "Top model" chip). The compat
    /// `llm_calls` relation must present exactly ONE row per call, with
    /// provider/model reconstructed from the winning provider attempt.
    #[test]
    fn llm_calls_projects_one_row_per_call_from_lifecycle_and_attempts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let calls_parquet = temp.path().join("call_fact.parquet");
        let attempts_parquet = temp.path().join("provider_attempts.parquet");
        let writer = Connection::open_in_memory().expect("writer");

        // One call: a 'started' (r1) + 'completed' (r2) call_fact lifecycle
        // pair, both with empty provider/model (as the pipeline writes them).
        writer
            .execute_batch(&format!(
                "COPY (SELECT * FROM (VALUES
                     (1700000000000::BIGINT, 'call-1'::VARCHAR, 'call_fact'::VARCHAR, 1::INTEGER, ''::VARCHAR, ''::VARCHAR, NULL::DOUBLE),
                     (1700000000000::BIGINT, 'call-1'::VARCHAR, 'call_fact'::VARCHAR, 2::INTEGER, ''::VARCHAR, ''::VARCHAR, 0.25::DOUBLE)
                   ) AS t(timestamp_ms, llm_call_id, record_kind, record_revision, provider, model, cost_usd))
                 TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(&calls_parquet.to_string_lossy())
            ))
            .expect("write call_fact fixture");

        // The winning provider attempt carries the real provider/model.
        writer
            .execute_batch(&format!(
                "COPY (SELECT 'call-1'::VARCHAR AS llm_call_id,
                              'openai'::VARCHAR AS provider,
                              'gpt-5.6'::VARCHAR AS model,
                              'succeeded'::VARCHAR AS attempt_terminal_state,
                              1::INTEGER AS provider_attempt_index)
                 TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(&attempts_parquet.to_string_lossy())
            ))
            .expect("write attempts fixture");

        let reader = Connection::open_in_memory().expect("reader");
        let attempts_src = format!(
            "'{}'",
            escape_sql_literal(&attempts_parquet.to_string_lossy())
        );
        install_legacy_llm_views(
            &reader,
            &format!("'{}'", escape_sql_literal(&calls_parquet.to_string_lossy())),
            Some(attempts_src.as_str()),
            "owner",
            "default",
            None,
        )
        .expect("install compatibility views");

        // Exactly one provider/model group, and it is the reconstructed real
        // model — not an empty/"unknown" lifecycle group, and not doubled.
        let (provider, model, rows): (Option<String>, Option<String>, i64) = reader
            .query_row(
                "SELECT provider, model, COUNT(*) FROM llm_calls GROUP BY provider, model",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("exactly one provider/model group");
        assert_eq!(provider.as_deref(), Some("openai"));
        assert_eq!(model.as_deref(), Some("gpt-5.6"));
        assert_eq!(rows, 1, "one row per call, no lifecycle duplication");

        let total: i64 = reader
            .query_row("SELECT COUNT(*) FROM llm_calls", [], |row| row.get(0))
            .expect("count");
        assert_eq!(total, 1, "the 'started' lifecycle row must not surface");
    }

    fn sql_path(path: &std::path::Path) -> String {
        format!("'{}'", escape_sql_literal(&path.to_string_lossy()))
    }

    fn disable_external_access(conn: &Connection) {
        conn.execute_batch(
            "SET enable_external_access = false; \
             SET autoinstall_known_extensions = false; \
             SET autoload_known_extensions = false;",
        )
        .expect("disable external access");
    }

    /// Regression: an unbounded `llm_calls` query returned DuckDB "Out of
    /// Memory" (953.5/953.6 MiB spill used) because install copied the whole
    /// selected history, through a materialized `SELECT *` CTE that carried
    /// every raw column, into a temp table before user SQL ran.
    ///
    /// The fixture holds ~77 MiB of unique restricted text (200k lifecycle
    /// rows, two per call) against a 64 MB memory + 64 MB spill budget; the
    /// old materializing shape fails on it under DuckDB 1.5.2. The view must
    /// answer an unbounded aggregate and a full-width ordered page, stay
    /// readable after external access is disabled, and leave every
    /// unregistered file unreachable.
    #[test]
    fn llm_calls_view_streams_history_larger_than_the_memory_budget() {
        let temp = tempfile::tempdir().expect("tempdir");
        let calls = temp.path().join("calls.parquet");
        let unregistered = temp.path().join("unregistered.parquet");
        let writer = Connection::open_in_memory().expect("writer");
        writer
            .execute_batch(&format!(
                "COPY (SELECT 1700000000000::BIGINT + i AS timestamp_ms,
                              'call-' || (i // 2)::VARCHAR AS llm_call_id,
                              'call_fact'::VARCHAR AS record_kind,
                              (1 + i % 2)::INTEGER AS record_revision,
                              'openai'::VARCHAR AS provider,
                              'gpt-5.6'::VARCHAR AS model,
                              0.001::DOUBLE AS cost_usd,
                              repeat(md5(i::VARCHAR), 12) AS reasoning_summary
                       FROM range(200000) t(i))
                 TO {} (FORMAT PARQUET);
                 COPY (SELECT 'other scope' AS secret) TO {} (FORMAT PARQUET);",
                sql_path(&calls),
                sql_path(&unregistered),
            ))
            .expect("write oversized history fixture");

        let reader = Connection::open_in_memory().expect("reader");
        reader
            .execute_batch(&format!(
                "SET threads = 1;
                 SET memory_limit = '64MB';
                 SET max_temp_directory_size = '64MB';
                 SET temp_directory = {};",
                sql_path(&temp.path().join("spill")),
            ))
            .expect("small test budget");
        install_legacy_llm_views(&reader, &sql_path(&calls), None, "owner", "default", None)
            .expect("install is lazy and cannot exhaust the budget");
        disable_external_access(&reader);

        let table_type: String = reader
            .query_row(
                "SELECT table_type FROM information_schema.tables WHERE table_name = 'llm_calls'",
                [],
                |row| row.get(0),
            )
            .expect("llm_calls relation");
        assert_eq!(table_type, "VIEW", "llm_calls must never be materialized");

        let (calls_seen, distinct_calls, provider): (i64, i64, String) = reader
            .query_row(
                "SELECT COUNT(*), COUNT(DISTINCT llm_call_id), ANY_VALUE(provider) FROM llm_calls",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("unbounded aggregate streams within the budget");
        assert_eq!(
            calls_seen, 100_000,
            "one row per call, no lifecycle doubling"
        );
        assert_eq!(distinct_calls, 100_000);
        assert_eq!(provider, "openai");

        let page_sql = bound_legacy_llm_query_rows(
            "SELECT * FROM llm_calls ORDER BY timestamp_ms DESC",
            super::super::duckdb_safety::ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
        );
        let mut statement = reader.prepare(&page_sql).expect("prepare page");
        let newest: Vec<i64> = statement
            .query_map([], |row| row.get::<_, i64>("timestamp_ms"))
            .expect("full-width ordered page within the budget")
            .collect::<Result<_, _>>()
            .expect("page rows");
        assert_eq!(
            newest.len(),
            super::super::duckdb_safety::ANALYTICS_DUCKDB_MAX_RESULT_ROWS
        );
        assert_eq!(newest[0], 1_700_000_199_999);

        assert!(reader
            .prepare("SELECT reasoning_summary FROM llm_calls")
            .is_err());
        assert!(
            reader
                .prepare(&format!(
                    "SELECT * FROM read_parquet({})",
                    sql_path(&unregistered)
                ))
                .is_err(),
            "files the server did not select must stay unreachable"
        );
        assert!(reader.prepare("SELECT * FROM glob('/tmp/*')").is_err());
        assert!(
            reader
                .execute_batch(&format!(
                    "SET allowed_paths = [{}]",
                    sql_path(&unregistered)
                ))
                .is_err(),
            "the registered file set cannot be widened once external access is off"
        );
    }

    /// Calls, provider attempts and embeddings installed on one connection
    /// each register their files without dropping the others', so every
    /// relation (and its reconciliation join) still reads after external
    /// access is disabled.
    #[test]
    fn compatibility_views_share_one_connection_after_external_access_is_disabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let calls = temp.path().join("calls.parquet");
        let attempts = temp.path().join("attempts.parquet");
        let embeddings = temp.path().join("embeddings.parquet");
        let writer = Connection::open_in_memory().expect("writer");
        writer
            .execute_batch(&format!(
                "COPY (SELECT * FROM (VALUES
                     (1700000000000::BIGINT, 'call-1'::VARCHAR, 'call_fact'::VARCHAR, 2::INTEGER, ''::VARCHAR, ''::VARCHAR)
                   ) AS t(timestamp_ms, llm_call_id, record_kind, record_revision, provider, model))
                 TO {} (FORMAT PARQUET);
                 COPY (SELECT 'call-1'::VARCHAR AS llm_call_id, 'openai'::VARCHAR AS provider,
                              'gpt-5.6'::VARCHAR AS model, 'succeeded'::VARCHAR AS attempt_terminal_state,
                              1::INTEGER AS provider_attempt_index)
                 TO {} (FORMAT PARQUET);
                 COPY (SELECT 1700000000000::BIGINT AS timestamp_ms, 'ollama'::VARCHAR AS provider,
                              'embed-model'::VARCHAR AS model, 7::BIGINT AS input_tokens)
                 TO {} (FORMAT PARQUET);",
                sql_path(&calls),
                sql_path(&attempts),
                sql_path(&embeddings),
            ))
            .expect("write fixtures");

        let reader = Connection::open_in_memory().expect("reader");
        let attempts_sql = format!("[{}]", sql_path(&attempts));
        install_legacy_llm_views(
            &reader,
            &format!("[{}]", sql_path(&calls)),
            Some(attempts_sql.as_str()),
            "owner",
            "default",
            Some(1_699_999_999_999),
        )
        .expect("install calls view");
        install_llm_embeddings_view(&reader, &sql_path(&embeddings))
            .expect("install embeddings view");
        disable_external_access(&reader);

        let model: Option<String> = reader
            .query_row("SELECT model FROM llm_calls", [], |row| row.get(0))
            .expect("calls view and its attempts join read after disable");
        assert_eq!(model.as_deref(), Some("gpt-5.6"));
        let tokens: i64 = reader
            .query_row("SELECT SUM(input_tokens) FROM llm_embeddings", [], |row| {
                row.get(0)
            })
            .expect("embeddings view reads after disable");
        assert_eq!(tokens, 7);
    }

    #[test]
    fn compatibility_scan_reads_only_projected_columns() {
        let columns = [
            "timestamp_ms",
            "llm_call_id",
            "record_kind",
            "record_revision",
            "reasoning_summary",
            "payload_checksum",
            "dt",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<HashSet<_>>();
        let defs = [
            CompatibilityColumn {
                name: "timestamp_ms",
                ty: "BIGINT",
                default_sql: "0::BIGINT".into(),
            },
            CompatibilityColumn {
                name: "llm_call_id",
                ty: "VARCHAR",
                default_sql: "NULL::VARCHAR".into(),
            },
            CompatibilityColumn {
                name: "model",
                ty: "VARCHAR",
                default_sql: "'unknown'::VARCHAR".into(),
            },
            CompatibilityColumn {
                name: "dt",
                ty: "VARCHAR",
                default_sql: "NULL::VARCHAR".into(),
            },
        ];
        assert_eq!(
            compatibility_source_columns_sql(&columns, &defs),
            "\"dt\", \"llm_call_id\", \"record_kind\", \"record_revision\", \"timestamp_ms\""
        );
        assert_eq!(
            compatibility_source_columns_sql(&HashSet::new(), &defs),
            "TRUE AS __compat_row"
        );
    }

    #[test]
    fn unbounded_compatibility_queries_get_a_row_limit() {
        assert_eq!(
            bound_legacy_llm_query_rows("SELECT * FROM llm_calls ORDER BY timestamp_ms DESC;", 10),
            "SELECT * FROM llm_calls ORDER BY timestamp_ms DESC\nLIMIT 10"
        );
        assert_eq!(
            bound_legacy_llm_query_rows("SELECT * FROM llm_calls -- newest first", 10),
            "SELECT * FROM llm_calls -- newest first\nLIMIT 10"
        );
        assert_eq!(
            bound_legacy_llm_query_rows(
                "SELECT model FROM llm_calls UNION ALL SELECT model FROM llm_embeddings",
                10
            ),
            "SELECT model FROM llm_calls UNION ALL SELECT model FROM llm_embeddings\nLIMIT 10"
        );
        for already_bounded in [
            "SELECT * FROM llm_calls LIMIT 5",
            "SELECT * FROM llm_calls ORDER BY timestamp_ms LIMIT 5 OFFSET 2",
            "SELECT * FROM llm_calls FETCH FIRST 5 ROWS ONLY",
            "not sql at all",
        ] {
            assert_eq!(
                bound_legacy_llm_query_rows(already_bounded, 10),
                already_bounded
            );
        }
        // A LIMIT inside a subquery does not bound the outer result.
        assert!(bound_legacy_llm_query_rows(
            "SELECT * FROM (SELECT * FROM llm_calls LIMIT 5) AS recent",
            10
        )
        .ends_with("\nLIMIT 10"));
    }

    #[test]
    fn llm_calls_lower_bound_reads_literal_and_relative_windows() {
        let now = 1_800_000_000_000_i64;
        let day = 86_400_000_i64;
        let slack = LLM_CALLS_RELATIVE_BOUND_SLACK_MS;
        let bound = |sql: &str| llm_calls_timestamp_lower_bound_ms(sql, now);

        assert_eq!(
            bound("SELECT COUNT(*) FROM llm_calls WHERE timestamp_ms >= 1700000000000"),
            Some(1_700_000_000_000)
        );
        // The `/llm` page's relative windows, as normalised by the API.
        assert_eq!(
            bound(
                "SELECT operation, SUM(cost_usd) FROM llm_calls \
                 WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) \
                 AND operation IS NOT NULL GROUP BY operation"
            ),
            Some(now - 30 * day - slack)
        );
        assert_eq!(
            bound(
                "SELECT 1 FROM llm_calls WHERE timestamp_ms > epoch_ms(now() - INTERVAL 6 HOURS)"
            ),
            Some(now - 6 * 3_600_000 - slack)
        );
        assert_eq!(
            bound("SELECT 1 FROM llm_calls WHERE timestamp_ms >= epoch_ms(current_date - INTERVAL 7 DAY)"),
            Some(now - 7 * day - slack)
        );
        assert_eq!(
            bound("SELECT 1 FROM llm_calls WHERE timestamp_ms >= epoch_ms(now()::TIMESTAMP - INTERVAL '2 days')"),
            Some(now - 2 * day - slack)
        );
        // Conjuncts, reversed comparisons, BETWEEN and aliases.
        assert_eq!(
            bound("SELECT 1 FROM llm_calls WHERE agent_id IS NOT NULL AND (timestamp_ms >= 5 AND timestamp_ms < 10)"),
            Some(5)
        );
        assert_eq!(
            bound("SELECT 1 FROM llm_calls WHERE timestamp_ms >= 5 AND timestamp_ms > 7"),
            Some(7)
        );
        assert_eq!(
            bound("SELECT 1 FROM llm_calls WHERE 5 <= timestamp_ms"),
            Some(5)
        );
        assert_eq!(
            bound("SELECT 1 FROM llm_calls WHERE timestamp_ms BETWEEN 5 AND 10"),
            Some(5)
        );
        assert_eq!(
            bound("SELECT 1 FROM llm_calls AS c WHERE c.timestamp_ms >= 5"),
            Some(5)
        );
        // Every arm bounded: the loosest arm wins.
        assert_eq!(
            bound(
                "SELECT 1 FROM llm_calls WHERE timestamp_ms >= 5 \
                 UNION ALL SELECT 1 FROM llm_calls WHERE timestamp_ms >= 3"
            ),
            Some(3)
        );
        // Today-vs-yesterday: one bounded CTE, every other SELECT reads it.
        assert_eq!(
            bound(
                "WITH scoped AS (SELECT *, COALESCE(NULLIF(model, ''), 'unknown') AS model_key \
                 FROM llm_calls WHERE timestamp_ms >= 100 AND timestamp_ms < 300 \
                 AND (COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate')), \
                 top_models AS (SELECT model_key FROM scoped WHERE timestamp_ms >= 200 GROUP BY model_key LIMIT 3) \
                 SELECT 'today' AS day, COUNT(*) FROM scoped WHERE timestamp_ms >= 200 \
                 UNION ALL SELECT 'today_model', COUNT(*) FROM scoped \
                 WHERE model_key IN (SELECT model_key FROM top_models) GROUP BY model_key"
            ),
            Some(100)
        );
    }

    #[test]
    fn llm_calls_lower_bound_refuses_anything_it_cannot_prove() {
        let now = 1_800_000_000_000_i64;
        for sql in [
            "SELECT COUNT(*) FROM llm_calls",
            "SELECT SUM(CASE WHEN timestamp_ms >= 5 THEN cost_usd END) FROM llm_calls",
            "SELECT COUNT(*) FILTER (WHERE timestamp_ms >= 5) FROM llm_calls",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= 5 OR agent_id = 'a'",
            "SELECT 1 FROM llm_calls WHERE NOT (timestamp_ms >= 5)",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms <= 5",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms BETWEEN 5 AND 10 AND timestamp_ms NOT BETWEEN 1 AND 2 AND FALSE OR TRUE",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= 5 \
             UNION ALL SELECT 1 FROM llm_calls",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= 5 \
             AND llm_call_id IN (SELECT llm_call_id FROM llm_calls)",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= (SELECT MIN(timestamp_ms) FROM llm_calls)",
            "SELECT 1 FROM llm_calls a JOIN llm_calls b USING (llm_call_id) WHERE a.timestamp_ms >= 5",
            "SELECT 1 FROM llm_calls, chat_session_cache_summary WHERE timestamp_ms >= 5",
            "SELECT * FROM chat_session_cache_summary",
            "SELECT 1 FROM llm_calls AS t(started_at_ms, timestamp_ms) WHERE timestamp_ms >= 5",
            "SELECT 1 FROM llm_calls c WHERE other.timestamp_ms >= 5",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= epoch_ms(to_timestamp(5))",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= epoch_ms(now() - INTERVAL 1 MONTH)",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= epoch_ms(now() + INTERVAL 1 DAY)",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= epoch(now() - INTERVAL 1 DAY)",
            "SELECT 1 FROM llm_calls WHERE timestamp_ms >= 1.5e12",
            "SELECT 1 FROM llm_embeddings WHERE timestamp_ms >= 5",
            "not sql at all",
        ] {
            assert_eq!(
                llm_calls_timestamp_lower_bound_ms(sql, now),
                None,
                "must not bound: {sql}"
            );
        }
    }
}
