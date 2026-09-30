//! Content-free compatibility boundary for the pre-canonical `llm_calls` lake.
//!
//! Phase 2 keeps the historical row-per-call dataset readable while canonical
//! facts are adopted. This module is the single boundary shared by REST and
//! `internal_data`: callers can query only the projected compatibility views,
//! never the raw Parquet relation or restricted legacy content columns.

use std::{collections::HashSet, ops::ControlFlow};

use anyhow::{anyhow, Result};
use duckdb::Connection;
use sqlparser::ast::{Expr, Statement, TableFactor, Visit, Visitor};

use super::llm_sql_guard::{
    is_read_only_select_query, is_read_only_select_query_node, parse_analytics_sql,
};

const ALLOWED_RELATIONS: &[&str] = &["llm_calls", "chat_session_cache_summary", "llm_embeddings"];
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
pub fn install_legacy_llm_views(
    conn: &Connection,
    read_source_sql: &str,
    attempts_source_sql: Option<&str>,
    principal: &str,
    workspace: &str,
    // Pulse fix: when the caller's query has a numeric `timestamp_ms >= N`
    // window, push it INTO the raw Parquet scan so the `llm_calls` temp table
    // only ever materializes the window's rows (previously the full
    // partition-pruned dataset was materialized before the day filter applied).
    // `None` preserves the prior full-materialization behaviour exactly.
    timestamp_lower_bound_ms: Option<i64>,
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
    let calls_relation = build_llm_calls_relation(
        read_source_sql,
        attempts_source_sql,
        &columns,
        timestamp_lower_bound_ms,
    );
    let view_sql = format!(
        "CREATE OR REPLACE TEMP TABLE llm_calls AS
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
    let view_sql = format!(
        "CREATE OR REPLACE TEMP TABLE llm_embeddings AS
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

/// Build the FROM relation for the compatibility `llm_calls` table: exactly one
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
fn build_llm_calls_relation(
    read_source_sql: &str,
    attempts_source_sql: Option<&str>,
    columns: &HashSet<String>,
    timestamp_lower_bound_ms: Option<i64>,
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
    let raw =
        format!("read_parquet({read_source_sql}, hive_partitioning = true, union_by_name = true)");
    if !columns.contains("record_kind") {
        // Pure legacy lake: no reconciliation CTE, so wrap the scan in a
        // subquery to attach the window filter when present.
        if window_filter.is_empty() {
            return raw;
        }
        return format!("(SELECT * FROM {raw}{window_filter})");
    }
    let win_att = match attempts_source_sql {
        Some(attempts) => format!(
            "win_att AS (
                 SELECT llm_call_id, provider, model FROM (
                     SELECT llm_call_id, provider, model,
                            ROW_NUMBER() OVER (
                                PARTITION BY llm_call_id
                                ORDER BY (attempt_terminal_state = 'succeeded') DESC,
                                         provider_attempt_index DESC NULLS LAST
                            ) AS rn
                     FROM read_parquet({attempts}, hive_partitioning = true, union_by_name = true)
                     WHERE COALESCE(NULLIF(model, ''), '') <> ''
                 ) WHERE rn = 1
             )"
        ),
        None => "win_att AS (SELECT NULL::VARCHAR AS llm_call_id, \
                 NULL::VARCHAR AS provider, NULL::VARCHAR AS model WHERE false)"
            .to_string(),
    };
    format!(
        "(
           WITH raw AS (SELECT * FROM {raw}{window_filter}),
                batch AS (SELECT * FROM raw WHERE record_kind IS NULL),
                completed AS (
                    SELECT * FROM raw
                    WHERE record_kind = 'call_fact' AND record_revision = 2
                      AND llm_call_id NOT IN (
                          SELECT llm_call_id FROM batch WHERE llm_call_id IS NOT NULL
                      )
                ),
                {win_att},
                projected AS (
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
}
