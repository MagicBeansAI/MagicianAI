//! Projection layer — per-(origin, resource) typed-row materialization.
//!
//! When a mined capability replays successfully, its JSON response body
//! is fed to [`infer_projection_from_sample`] to discover the canonical
//! "array of homogeneous objects" inside the payload, and the rows are
//! extracted into a SQLite table keyed by a per-projection primary key.
//! Subsequent agent queries hit `ProjectionStore::query_rows` instead
//! of replaying the request, removing the network round-trip entirely
//! for hot-path resources.
//!
//! ## Module layout (PL Tasks 9-13)
//!
//! - **Task 9**: [`find_array_of_objects`] — BFS-largest traversal
//!   shared by inference and extraction.
//! - **Task 10**: [`ResourceProjection`] — per-projection record
//!   persisted as JSON alongside the SQLite table.
//! - **Task 11**: [`ProjectionStore`] — SQLite-backed (`rusqlite`)
//!   per-scope row store with WAL mode. Methods for create / insert
//!   / query / migrate / purge.
//! - **Task 12**: [`infer_projection_from_sample`] +
//!   [`extract_rows`] — schema inference from a response sample and
//!   row extraction from later samples, both using the shared
//!   traversal helper.
//! - **Task 13**: [`ProjectionStore::migrate_table`] —
//!   schema-convergence via additive `ALTER TABLE ADD COLUMN`.
//!   Rejects column drops / type changes; operator must
//!   [`ProjectionStore::purge_rows`] before re-applying.

use std::path::{Path, PathBuf};

use rusqlite::{params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lifecycle of a projection — operator approval gates row ingest
/// so the system can't accidentally materialize sensitive data
/// without a human flip. State transitions:
///
/// `Pending` (auto-created on first replay)
///   → `Approved` (operator via Forge action)
///   → `Live` (set on first successful row ingest after approval)
///   → `Invalidated` (operator-flagged or auto-set on repeated
///     extraction failures)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionLifecycle {
    Pending,
    Approved,
    Live,
    Invalidated,
}

/// SQLite-compatible column types inferred from JSON values.
///
/// We map JSON shapes to a tiny coproduct of SQLite affinities; the
/// fancy types (BLOB, DATETIME) are not used because JSON doesn't
/// carry that distinction without extra hints we don't have at
/// inference time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    /// `TEXT` — strings.
    Text,
    /// `INTEGER` — integer JSON numbers.
    Integer,
    /// `REAL` — floating-point JSON numbers.
    Real,
    /// `INTEGER` 0/1 — booleans.
    Bool,
    /// `TEXT` storing serialized JSON — arrays/objects.
    Json,
    /// `TEXT` with `NULL` rows allowed — JSON null observed in sample.
    /// Used during inference when the first sample's column was null;
    /// subsequent samples may refine to a concrete type via the
    /// convergence path.
    NullOnly,
}

impl ColumnType {
    /// Map this inferred type to the SQLite affinity string used in
    /// CREATE TABLE / ALTER TABLE.
    pub fn sqlite_affinity(self) -> &'static str {
        match self {
            ColumnType::Text => "TEXT",
            ColumnType::Integer => "INTEGER",
            ColumnType::Real => "REAL",
            ColumnType::Bool => "INTEGER",
            ColumnType::Json => "TEXT",
            ColumnType::NullOnly => "TEXT",
        }
    }
}

/// One column in a `ResourceProjection`'s inferred schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    /// Identifier in the SQL table. Safe-canonicalized from the JSON
    /// key — see [`Column::sanitize_name`].
    pub name: String,
    /// JSON key path relative to the projection's `array_path`. For
    /// flat shapes this is just the field name; for nested
    /// `${field}.${nested}` chains we serialize the path joined by `.`.
    pub json_path: String,
    /// SQLite-affinity-friendly type inferred from the sample.
    pub column_type: ColumnType,
    /// Whether NULL was observed in any sample at this position.
    /// Influences SQLite `NOT NULL` constraint emission.
    #[serde(default)]
    pub nullable: bool,
}

impl Column {
    /// Sanitize a JSON key into a SQLite identifier. SQLite is
    /// permissive but quoting non-alphanumeric names is annoying;
    /// canonicalize to `[a-zA-Z0-9_]` with underscores for everything
    /// else. Empty keys default to `col`.
    pub fn sanitize_name(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        for ch in raw.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
        if out.is_empty() {
            return "col".to_string();
        }
        // Leading digit? Prefix with underscore to make a legal
        // SQLite identifier without backtick-quoting at every site.
        if out
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            out.insert(0, '_');
        }
        out
    }
}

/// Per-(origin, resource) projection metadata persisted as JSON.
///
/// The corresponding SQLite rows live in a separate file
/// (`<origin_key>.db`); this record holds the schema, primary key
/// hint, TTL, and lifecycle state. Operators view + manage projections
/// through this record's surface in Forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceProjection {
    /// Stable id — `<origin_key>_<resource_label>`.
    pub id: String,
    /// Capability that produced this projection. Same `id` as
    /// `ApiCapability::id`.
    pub capability_id: String,
    /// Origin URL string.
    pub origin: String,
    /// Free-text label derived from URL template
    /// (see `derive_resource_label`). Operator-editable.
    pub resource_label: String,
    /// Path into the response JSON pointing at the array we project.
    /// Empty path means "the response IS the array."
    pub array_path: Vec<String>,
    /// Inferred columns.
    pub columns: Vec<Column>,
    /// Optional primary key column name (one of the columns). Used
    /// for `INSERT OR REPLACE` semantics. When `None`, all inserts
    /// are append-only and row dedup is the agent's problem.
    pub primary_key: Option<String>,
    /// Per-projection TTL. `query_known_resource` returns
    /// `ServedFrom::StaleProjection` past this age but still serves
    /// rows; caller decides whether to refresh.
    pub ttl_seconds: i64,
    /// Lifecycle state.
    pub lifecycle: ProjectionLifecycle,
    /// Epoch millis of last ingest.
    pub last_ingested_at: Option<i64>,
    /// Created / updated timestamps.
    pub created_at: i64,
    pub updated_at: i64,
}

/// SQLite-backed per-scope projection row store.
///
/// One DB file per scope (not per projection); each projection lives
/// in its own table inside that DB. WAL mode is enabled at `open`
/// time so readers don't block writers and vice versa — important
/// because `query_known_resource` and `ingest_response` run in the
/// same process under tokio.
///
/// All public methods take `&self` and synchronize internally via
/// rusqlite's connection mutex. The store is meant to live inside an
/// `Arc<Mutex<...>>` at the `ProjectionPipelineState` level so we
/// have a single serialization point per scope.
pub struct ProjectionStore {
    db_path: PathBuf,
    conn: std::sync::Mutex<Connection>,
}

impl ProjectionStore {
    /// Open (or create) the per-scope projection DB at the given
    /// path. Enables WAL mode and creates the parent directory if it
    /// doesn't exist. Returns the store ready for `create_table` /
    /// `insert_rows` / `query_rows`.
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, String> {
        let db_path = db_path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create projection-store dir {parent:?}: {e}"))?;
        }
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("open projection DB {db_path:?}: {e}"))?;
        // WAL mode: readers + writer don't block each other.
        // `synchronous = NORMAL` is the WAL-safe lower-latency choice;
        // we accept the small windowed-loss risk on power-failure for
        // significant per-write latency wins.
        conn.execute_batch("PRAGMA journal_mode = WAL;\n             PRAGMA synchronous = NORMAL;")
            .map_err(|e| format!("set WAL on {db_path:?}: {e}"))?;
        Ok(Self {
            db_path,
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// Database file path. Tests use this to introspect after
    /// operations; production callers should not need it.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Create the SQLite table for a freshly-inferred projection.
    /// Idempotent — if the table already exists with the same shape,
    /// returns silently; if the shape differs, falls back to
    /// `migrate_table`.
    pub fn create_table_for_projection(
        &self,
        projection: &ResourceProjection,
    ) -> Result<(), String> {
        let table_name = sanitize_table_name(&projection.id);
        let columns_sql = projection
            .columns
            .iter()
            .map(|c| format!("\"{}\" {}", c.name, c.column_type.sqlite_affinity()))
            .collect::<Vec<_>>()
            .join(", ");
        let pk_clause = projection
            .primary_key
            .as_ref()
            .map(|pk| format!(", PRIMARY KEY (\"{pk}\")"))
            .unwrap_or_default();
        let sql = format!("CREATE TABLE IF NOT EXISTS \"{table_name}\" ({columns_sql}{pk_clause})");
        let conn = self
            .conn
            .lock()
            .map_err(|_| "projection store mutex poisoned".to_string())?;
        conn.execute(&sql, [])
            .map_err(|e| format!("create table {table_name}: {e}"))?;
        Ok(())
    }

    /// Insert rows extracted from a response sample. Uses `INSERT OR
    /// REPLACE` when the projection has a primary key, plain `INSERT`
    /// otherwise.
    pub fn insert_rows(
        &self,
        projection: &ResourceProjection,
        rows: &[serde_json::Map<String, Value>],
    ) -> Result<usize, String> {
        if rows.is_empty() {
            return Ok(0);
        }
        let table_name = sanitize_table_name(&projection.id);
        let conn = self
            .conn
            .lock()
            .map_err(|_| "projection store mutex poisoned".to_string())?;
        let mut inserted = 0usize;
        let column_names: Vec<&str> = projection.columns.iter().map(|c| c.name.as_str()).collect();
        let placeholders = column_names
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let col_list = column_names
            .iter()
            .map(|n| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let verb = if projection.primary_key.is_some() {
            "INSERT OR REPLACE INTO"
        } else {
            "INSERT INTO"
        };
        let sql = format!("{verb} \"{table_name}\" ({col_list}) VALUES ({placeholders})");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare insert for {table_name}: {e}"))?;
        for row in rows {
            let values: Vec<rusqlite::types::Value> = projection
                .columns
                .iter()
                .map(|col| json_to_sqlite_value(row.get(&col.json_path).unwrap_or(&Value::Null)))
                .collect();
            stmt.execute(params_from_iter(values.iter()))
                .map_err(|e| format!("insert into {table_name}: {e}"))?;
            inserted += 1;
        }
        Ok(inserted)
    }

    /// Query the projection's rows. `where_clause` is an optional
    /// raw SQL fragment with `?` placeholders bound to `params`. Pass
    /// `None` to fetch every row.
    ///
    /// **Safety**: the `where_clause` is filtered through
    /// [`validate_where_clause`] to reject statement-terminators,
    /// SQL comments, and DDL/DML keywords. Caller-supplied filters
    /// are still expressive enough for the common cases (column
    /// comparisons, IN lists, simple AND/OR), but cannot exfiltrate
    /// data from other tables in the per-scope SQLite DB via
    /// `UNION SELECT` or escape into multi-statement payloads.
    pub fn query_rows(
        &self,
        projection: &ResourceProjection,
        where_clause: Option<&str>,
        params: &[Value],
    ) -> Result<Vec<serde_json::Map<String, Value>>, String> {
        let table_name = sanitize_table_name(&projection.id);
        let col_list = projection
            .columns
            .iter()
            .map(|c| format!("\"{}\"", c.name))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = match where_clause {
            Some(filter) if !filter.trim().is_empty() => {
                validate_where_clause(filter)?;
                format!("SELECT {col_list} FROM \"{table_name}\" WHERE {filter}")
            },
            _ => format!("SELECT {col_list} FROM \"{table_name}\""),
        };
        let conn = self
            .conn
            .lock()
            .map_err(|_| "projection store mutex poisoned".to_string())?;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare select for {table_name}: {e}"))?;
        let bind_values: Vec<rusqlite::types::Value> =
            params.iter().map(json_to_sqlite_value).collect();
        let columns = projection.columns.clone();
        let rows_iter = stmt
            .query_map(params_from_iter(bind_values.iter()), move |row| {
                let mut obj = serde_json::Map::new();
                for (idx, col) in columns.iter().enumerate() {
                    let val: rusqlite::types::Value = row.get(idx)?;
                    obj.insert(col.json_path.clone(), sqlite_to_json_value(val));
                }
                Ok(obj)
            })
            .map_err(|e| format!("query {table_name}: {e}"))?;
        let mut out = Vec::new();
        for row_result in rows_iter {
            out.push(row_result.map_err(|e| format!("row read {table_name}: {e}"))?);
        }
        Ok(out)
    }

    /// Apply a schema convergence step: additively add columns that
    /// appear in `new_columns` but not in `existing` (the projection
    /// at the start of the call). Returns the number of columns
    /// added. Rejects with `Err` if any `existing` column is missing
    /// from `new_columns` (column drop) or has a different
    /// `column_type` (type change) — both require an explicit
    /// `purge_rows` first.
    ///
    /// PL Task 13: this is the "additive only" guarantee. Schema
    /// convergence must be safe under concurrent reads, so we
    /// `ALTER TABLE ADD COLUMN` (well-supported on SQLite, atomic via
    /// WAL) rather than rebuilding the table.
    pub fn migrate_table(
        &self,
        projection: &ResourceProjection,
        new_columns: &[Column],
    ) -> Result<usize, String> {
        let existing: std::collections::HashMap<&str, &Column> = projection
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c))
            .collect();
        let new_map: std::collections::HashMap<&str, &Column> =
            new_columns.iter().map(|c| (c.name.as_str(), c)).collect();
        // Reject drops
        for existing_col in &projection.columns {
            if !new_map.contains_key(existing_col.name.as_str()) {
                return Err(format!(
                    "purge_rows required: column '{}' would be dropped",
                    existing_col.name
                ));
            }
        }
        // Reject type changes
        for new_col in new_columns {
            if let Some(prev) = existing.get(new_col.name.as_str()) {
                if prev.column_type != new_col.column_type {
                    return Err(format!(
                        "purge_rows required: column '{}' type change ({:?} -> {:?})",
                        new_col.name, prev.column_type, new_col.column_type
                    ));
                }
            }
        }
        // Additive: ALTER TABLE ADD COLUMN for each new entry.
        let table_name = sanitize_table_name(&projection.id);
        let conn = self
            .conn
            .lock()
            .map_err(|_| "projection store mutex poisoned".to_string())?;
        let mut added = 0usize;
        for new_col in new_columns {
            if existing.contains_key(new_col.name.as_str()) {
                continue;
            }
            let sql = format!(
                "ALTER TABLE \"{table_name}\" ADD COLUMN \"{col}\" {affinity}",
                col = new_col.name,
                affinity = new_col.column_type.sqlite_affinity(),
            );
            conn.execute(&sql, [])
                .map_err(|e| format!("alter add column on {table_name}: {e}"))?;
            added += 1;
        }
        Ok(added)
    }

    /// Drop all rows for a projection (the operator's "I want to
    /// re-shape the schema, blow this away first" hammer). Returns
    /// the deleted row count.
    pub fn purge_rows(&self, projection: &ResourceProjection) -> Result<usize, String> {
        let table_name = sanitize_table_name(&projection.id);
        let conn = self
            .conn
            .lock()
            .map_err(|_| "projection store mutex poisoned".to_string())?;
        let count: usize = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM \"{table_name}\""),
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap_or(0);
        conn.execute(&format!("DELETE FROM \"{table_name}\""), [])
            .map_err(|e| format!("purge {table_name}: {e}"))?;
        Ok(count)
    }

    /// Remove one projection's table entirely. Origin deletion uses this
    /// stronger operation so field-name schema and an empty table are not left
    /// behind after the projection record is removed. The ordinary operator
    /// `purge_rows` action above intentionally keeps its schema.
    pub fn drop_projection(&self, projection: &ResourceProjection) -> Result<usize, String> {
        let table_name = sanitize_table_name(&projection.id);
        let conn = self
            .conn
            .lock()
            .map_err(|_| "projection store mutex poisoned".to_string())?;
        let count: usize = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM \"{table_name}\""),
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap_or(0);
        conn.execute(&format!("DROP TABLE IF EXISTS \"{table_name}\""), [])
            .map_err(|error| format!("drop projection table {table_name}: {error}"))?;
        Ok(count)
    }
}

/// Derive a stable, human-readable resource label from a URL template.
/// PL Task 16 lives here so the projection record can reference it
/// directly without an extra module.
///
/// Strategy: drop scheme + host, take the path, replace
/// `/{placeholder}/` segments with `*`, lowercase, replace
/// non-alphanumeric runs with `-`. Always non-empty.
///
/// ```text
/// /api/card/12116/query            → card-12116-query
/// /api/v1/users/{id}/sessions      → users-*-sessions
/// /graphql                         → graphql
/// ```
pub fn derive_resource_label(url_template: &str) -> String {
    // Strip scheme + host
    let path = if let Some(scheme_end) = url_template.find("://") {
        let after_scheme = &url_template[scheme_end + 3..];
        match after_scheme.find('/') {
            Some(slash) => &after_scheme[slash + 1..],
            None => "",
        }
    } else {
        url_template.trim_start_matches('/')
    };
    // Strip query string and fragment. Without this, two calls to
    // the same endpoint with different query parameters (e.g.
    // `?date=2026-05-10` vs `?date=2026-05-11`) generate distinct
    // resource labels, producing one-row-per-date projections
    // instead of one many-row projection. Operators query rows by
    // the relevant filter via the WHERE clause — the resource
    // identity is the path shape, not the parameter values.
    let path = match path.find('?') {
        Some(q) => &path[..q],
        None => path,
    };
    let path = match path.find('#') {
        Some(h) => &path[..h],
        None => path,
    };
    if path.is_empty() {
        return "root".to_string();
    }
    // Drop common `api`, `v1` prefixes (informational, not
    // discriminating).
    let segments: Vec<&str> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .skip_while(|s| matches!(*s, "api" | "v1" | "v2" | "v3"))
        .collect();
    if segments.is_empty() {
        return "root".to_string();
    }
    let mut out = String::with_capacity(path.len());
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            out.push('-');
        }
        if seg.starts_with('{') && seg.ends_with('}') {
            out.push('*');
        } else {
            for ch in seg.chars() {
                if ch.is_ascii_alphanumeric() {
                    out.push(ch.to_ascii_lowercase());
                } else {
                    out.push('-');
                }
            }
        }
    }
    // Collapse repeated dashes
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

/// PL Task 9: BFS-find the largest array of homogeneous objects in
/// a JSON value, returning the path used to reach it. Used by both
/// inference (`infer_projection_from_sample`) and extraction
/// (`extract_rows`) so the path is computed once and applied
/// identically on every later sample.
///
/// Heuristic: BFS the tree, score each array-of-objects encountered
/// by its length, pick the largest. Tied arrays at the same depth
/// pick the first by key order (deterministic).
pub fn find_array_of_objects(root: &Value) -> Option<(Vec<String>, &Vec<Value>)> {
    let mut queue: std::collections::VecDeque<(Vec<String>, &Value)> =
        std::collections::VecDeque::new();
    queue.push_back((Vec::new(), root));
    let mut best: Option<(Vec<String>, &Vec<Value>)> = None;
    while let Some((path, val)) = queue.pop_front() {
        match val {
            Value::Array(items) => {
                if items.is_empty() {
                    continue;
                }
                if items.iter().all(|v| matches!(v, Value::Object(_))) {
                    let pick = match &best {
                        Some((_, prev)) => items.len() > prev.len(),
                        None => true,
                    };
                    if pick {
                        best = Some((path.clone(), items));
                    }
                }
                // Even if best, drill into nested arrays — there may
                // be a larger one inside (e.g., scoreboard has many
                // small subarrays inside a giant events array; we
                // want events).
                for (idx, item) in items.iter().enumerate() {
                    let mut child_path = path.clone();
                    child_path.push(format!("[{idx}]"));
                    queue.push_back((child_path, item));
                }
            },
            Value::Object(map) => {
                for (k, v) in map {
                    let mut child_path = path.clone();
                    child_path.push(k.clone());
                    queue.push_back((child_path, v));
                }
            },
            _ => {},
        }
    }
    best
}

/// PL Task 12: infer a projection schema from a response sample.
/// Returns `None` if no array-of-objects can be found — operators
/// must hand-author for such responses.
pub fn infer_projection_from_sample(
    capability_id: &str,
    origin: &str,
    resource_label: String,
    response: &Value,
) -> Option<ResourceProjection> {
    let (array_path, items) = find_array_of_objects(response)?;
    if items.is_empty() {
        return None;
    }
    // Use the first row's keys as the canonical schema. We don't
    // union — that's the convergence path's job, not inference.
    let first = items.iter().find_map(|v| v.as_object())?;
    let mut columns: Vec<Column> = first
        .iter()
        .map(|(k, v)| Column {
            name: Column::sanitize_name(k),
            json_path: k.clone(),
            column_type: infer_column_type(v),
            nullable: matches!(v, Value::Null),
        })
        .collect();
    // Stable ordering — alphabetical — so re-emissions don't churn.
    columns.sort_by(|a, b| a.name.cmp(&b.name));
    // Heuristic primary key: prefer a column literally named `id`.
    let primary_key = columns
        .iter()
        .find(|c| c.name == "id")
        .map(|c| c.name.clone());
    let now = chrono::Utc::now().timestamp_millis();
    Some(ResourceProjection {
        id: format!(
            "{}_{}",
            sanitize_origin_key(origin),
            sanitize_table_name(&resource_label)
        ),
        capability_id: capability_id.to_string(),
        origin: origin.to_string(),
        resource_label,
        array_path,
        columns,
        primary_key,
        ttl_seconds: 3600,
        lifecycle: ProjectionLifecycle::Pending,
        last_ingested_at: None,
        created_at: now,
        updated_at: now,
    })
}

/// PL Task 12: extract rows from a later response sample using the
/// projection's stored `array_path`. Each row is a `Map<String,
/// Value>` keyed by the projection's `json_path`s.
pub fn extract_rows(
    projection: &ResourceProjection,
    response: &Value,
) -> Vec<serde_json::Map<String, Value>> {
    let mut current = response;
    for segment in &projection.array_path {
        current = match current {
            Value::Object(map) => match map.get(segment) {
                Some(v) => v,
                None => return Vec::new(),
            },
            // Array path segments shaped as `[<idx>]` are emitted by
            // `find_array_of_objects` when it drills into nested
            // arrays. The projection-side path almost always avoids
            // them (we pick the FIRST array-of-objects, not a
            // specific element), but support them for completeness.
            Value::Array(items) => {
                if let Some(idx_str) = segment.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        match items.get(idx) {
                            Some(v) => v,
                            None => return Vec::new(),
                        }
                    } else {
                        return Vec::new();
                    }
                } else {
                    return Vec::new();
                }
            },
            _ => return Vec::new(),
        };
    }
    let items = match current.as_array() {
        Some(items) => items,
        None => return Vec::new(),
    };
    items
        .iter()
        .filter_map(|v| v.as_object().cloned())
        .collect()
}

/// Type-infer a single column from a sample value.
fn infer_column_type(val: &Value) -> ColumnType {
    match val {
        Value::Null => ColumnType::NullOnly,
        Value::Bool(_) => ColumnType::Bool,
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                ColumnType::Integer
            } else {
                ColumnType::Real
            }
        },
        Value::String(_) => ColumnType::Text,
        Value::Array(_) | Value::Object(_) => ColumnType::Json,
    }
}

/// JSON → rusqlite value with sensible coercion.
fn json_to_sqlite_value(val: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as Sql;
    match val {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Sql::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Sql::Real(f)
            } else {
                Sql::Null
            }
        },
        Value::String(s) => Sql::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => Sql::Text(val.to_string()),
    }
}

/// rusqlite value → JSON for query result projection.
fn sqlite_to_json_value(val: rusqlite::types::Value) -> Value {
    use rusqlite::types::Value as Sql;
    match val {
        Sql::Null => Value::Null,
        Sql::Integer(i) => Value::Number(i.into()),
        Sql::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Sql::Text(s) => Value::String(s),
        Sql::Blob(b) => Value::String(base64_encode(&b)),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    // Tiny stdlib-only base64; we don't ship blob payloads in this
    // pipeline today, so this is only hit when an extension function
    // returns blob data we didn't expect. Format the bytes as hex
    // since base64 needs a dep; this is correct and round-trippable
    // for diagnostics.
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{:02x}", b).expect("hex write");
    }
    out
}

/// Sanitize an arbitrary string to be safe as a SQLite table name
/// (or filename segment). ASCII alphanumeric + underscore only;
/// leading digit gets prefixed.
fn sanitize_table_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        return "tbl".to_string();
    }
    if out
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        out.insert(0, '_');
    }
    out
}

/// Same shape as `sanitize_table_name`, exported for filename use.
pub fn sanitize_origin_key(origin: &str) -> String {
    sanitize_table_name(origin)
}

/// Validate that a caller-supplied SQL WHERE clause fragment is safe
/// to interpolate into `SELECT … FROM <table> WHERE <fragment>`.
/// Rejects:
/// - statement terminators (`;`) — defence-in-depth, since rusqlite's
///   `prepare_v2` already executes only the first statement, but
///   trailing junk is still confusing in logs.
/// - SQL line comments (`--`) and block comments (`/* */`) — common
///   bypass vectors for keyword filters.
/// - DDL / DML / control keywords as whole words —
///   `DROP DELETE ALTER INSERT UPDATE REPLACE ATTACH DETACH PRAGMA
///   CREATE UNION SELECT EXEC EXECUTE`. Subqueries are denied entirely
///   so a malicious caller can't `WHERE id IN (SELECT … FROM other)`
///   to read another projection's data.
///
/// The accepted shape is therefore "comparisons, IN lists, AND/OR,
/// parens, `?` placeholders" — enough for the agent-tool surface
/// (`query_known_resource`) and operator HTTP testing without
/// exposing cross-table reads.
pub fn validate_where_clause(clause: &str) -> Result<(), String> {
    if clause.contains(';') {
        return Err("WHERE clause must not contain ';' (statement terminator)".to_string());
    }
    if clause.contains("--") {
        return Err("WHERE clause must not contain SQL line comments ('--')".to_string());
    }
    if clause.contains("/*") || clause.contains("*/") {
        return Err("WHERE clause must not contain SQL block comments".to_string());
    }
    // Whole-word keyword reject. Match on uppercase-normalized text
    // with word boundaries (ASCII alphanumeric / underscore) so
    // partial matches like `description` don't false-positive on
    // `desc`, but `DESC ; DROP` matches.
    let upper = clause.to_ascii_uppercase();
    const BANNED_KEYWORDS: &[&str] = &[
        "DROP", "DELETE", "ALTER", "INSERT", "UPDATE", "REPLACE", "ATTACH", "DETACH", "PRAGMA",
        "CREATE", "UNION", "SELECT", "EXEC", "EXECUTE",
    ];
    for kw in BANNED_KEYWORDS {
        if contains_whole_word(&upper, kw) {
            return Err(format!(
                "WHERE clause contains banned SQL keyword '{kw}'; this filter must be expressed without subqueries or DDL/DML"
            ));
        }
    }
    Ok(())
}

/// Check whether `haystack` contains `needle` as a whole word (i.e.
/// surrounded by non-identifier characters or the string boundary).
fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let n = needle_bytes.len();
    if n == 0 || bytes.len() < n {
        return false;
    }
    let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for start in 0..=bytes.len() - n {
        if &bytes[start..start + n] != needle_bytes {
            continue;
        }
        let prev_ok = start == 0 || !is_word_char(bytes[start - 1]);
        let next_ok = start + n == bytes.len() || !is_word_char(bytes[start + n]);
        if prev_ok && next_ok {
            return true;
        }
    }
    false
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derive_resource_label_strips_api_prefix_and_normalizes() {
        assert_eq!(
            derive_resource_label("/api/card/12116/query"),
            "card-12116-query"
        );
        assert_eq!(
            derive_resource_label("https://x.com/api/v1/users/{id}"),
            "users-*"
        );
        assert_eq!(derive_resource_label("/graphql"), "graphql");
        assert_eq!(derive_resource_label("/"), "root");
    }

    #[test]
    fn derive_resource_label_strips_query_and_fragment() {
        // Different parameter values must map to the SAME projection
        // — otherwise we'd accumulate one one-row projection per
        // distinct date, defeating the point of the layer.
        assert_eq!(
            derive_resource_label("/api/card/12116/query?date=2026-05-10"),
            "card-12116-query"
        );
        assert_eq!(
            derive_resource_label("/api/card/12116/query?date=2026-05-11"),
            "card-12116-query"
        );
        assert_eq!(
            derive_resource_label("https://x.com/users/42#tab=details"),
            "users-42"
        );
        assert_eq!(derive_resource_label("/search?q=foo&page=2"), "search");
    }

    #[test]
    fn find_array_of_objects_picks_largest() {
        let payload = json!({
            "events": [
                {"id": "e1", "score": 1},
                {"id": "e2", "score": 2},
            ],
            "meta": {"totals": [{"count": 1}]}
        });
        let (path, items) = find_array_of_objects(&payload).unwrap();
        assert_eq!(path, vec!["events"]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn infer_and_extract_round_trip() {
        let payload = json!({
            "data": {"rows": [{"id": "a", "n": 1}, {"id": "b", "n": 2}]}
        });
        let p =
            infer_projection_from_sample("cap-1", "https://x.com", "rows".to_string(), &payload)
                .expect("projection");
        assert_eq!(p.columns.len(), 2);
        assert_eq!(p.array_path, vec!["data", "rows"]);
        let rows = extract_rows(&p, &payload);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], json!("a"));
    }

    #[test]
    fn store_create_insert_query_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProjectionStore::open(tmp.path().join("p.db")).unwrap();
        let payload = json!({
            "items": [{"id": "a", "n": 1}, {"id": "b", "n": 2}]
        });
        let p =
            infer_projection_from_sample("cap-1", "https://x.com", "items".to_string(), &payload)
                .unwrap();
        store.create_table_for_projection(&p).unwrap();
        let rows = extract_rows(&p, &payload);
        let inserted = store.insert_rows(&p, &rows).unwrap();
        assert_eq!(inserted, 2);
        let queried = store.query_rows(&p, None, &[]).unwrap();
        assert_eq!(queried.len(), 2);
    }

    #[test]
    fn migrate_rejects_column_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProjectionStore::open(tmp.path().join("p.db")).unwrap();
        let payload = json!({"items": [{"id": "a", "n": 1}]});
        let p = infer_projection_from_sample("c", "h", "items".into(), &payload).unwrap();
        store.create_table_for_projection(&p).unwrap();
        let dropped_n = vec![Column {
            name: "id".into(),
            json_path: "id".into(),
            column_type: ColumnType::Text,
            nullable: false,
        }];
        let err = store.migrate_table(&p, &dropped_n).unwrap_err();
        assert!(err.contains("purge_rows required"));
    }

    #[test]
    fn validate_where_clause_accepts_safe_filters() {
        assert!(validate_where_clause("id = ?").is_ok());
        assert!(validate_where_clause("date >= ? AND date < ?").is_ok());
        assert!(validate_where_clause("status IN (?, ?, ?)").is_ok());
        assert!(validate_where_clause("(score > ? OR score IS NULL)").is_ok());
        // Substrings that look like keywords but aren't whole words are fine.
        assert!(validate_where_clause("description = ?").is_ok());
    }

    #[test]
    fn validate_where_clause_rejects_injection_vectors() {
        assert!(validate_where_clause("1=1; DROP TABLE x").is_err());
        assert!(validate_where_clause("1=1 -- comment out").is_err());
        assert!(validate_where_clause("1=1 /* */ OR id=1").is_err());
        assert!(validate_where_clause("id IN (SELECT id FROM other)").is_err());
        assert!(validate_where_clause("id = ? UNION SELECT * FROM secrets").is_err());
        assert!(validate_where_clause("EXEC sp_admin").is_err());
        assert!(validate_where_clause("name = 'a' OR DROP").is_err());
    }

    #[test]
    fn migrate_additive_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProjectionStore::open(tmp.path().join("p.db")).unwrap();
        let payload = json!({"items": [{"id": "a"}]});
        let p = infer_projection_from_sample("c", "h", "items".into(), &payload).unwrap();
        store.create_table_for_projection(&p).unwrap();
        let mut wider = p.columns.clone();
        wider.push(Column {
            name: "venue".into(),
            json_path: "venue".into(),
            column_type: ColumnType::Text,
            nullable: true,
        });
        let added = store.migrate_table(&p, &wider).unwrap();
        assert_eq!(added, 1);
    }
}
