//! Convenience views over the schemaless `events` table.
//! Run on startup via `create_views()`. Safe to re-run (CREATE OR REPLACE).

use anyhow::{Context, Result};
use duckdb::Connection;
use tracing::debug;

/// All view DDL statements. Each is a CREATE OR REPLACE VIEW — idempotent.
const VIEW_DDLS: &[&str] = &[
    // Typed view over log events
    r#"CREATE OR REPLACE VIEW logs AS
    SELECT
        timestamp,
        payload->>'level' as level,
        payload->>'message' as message,
        payload->>'target' as target,
        source
    FROM events
    WHERE event_type = 'log'"#,
    // Typed view over bot log events
    r#"CREATE OR REPLACE VIEW bot_logs AS
    SELECT
        timestamp,
        payload->>'bot_name' as bot_name,
        payload->>'stream' as stream,
        payload->>'line' as line,
        source
    FROM events
    WHERE event_type = 'bot_log'"#,
    // Typed view over chat messages
    r#"CREATE OR REPLACE VIEW chat_messages AS
    SELECT
        timestamp,
        payload->>'session_id' as session_id,
        payload->>'direction' as direction,
        payload->>'content_text' as content_text,
        source
    FROM events
    WHERE event_type = 'chat_message'"#,
    // Typed view over chat sessions
    r#"CREATE OR REPLACE VIEW chat_sessions AS
    SELECT
        timestamp,
        payload->>'session_id' as session_id,
        payload->>'principal' as principal,
        payload->>'agent_id' as agent_id,
        payload->>'status' as status,
        source
    FROM events
    WHERE event_type = 'chat_session'"#,
    // Typed view over task executions
    r#"CREATE OR REPLACE VIEW task_executions AS
    SELECT
        timestamp,
        payload->>'task_id' as task_id,
        payload->>'execution_id' as execution_id,
        payload->>'status' as status,
        CAST(payload->>'duration_ms' AS INTEGER) as duration_ms,
        source
    FROM events
    WHERE event_type = 'task_execution'"#,
    // Typed view over task steps
    r#"CREATE OR REPLACE VIEW task_steps AS
    SELECT
        timestamp,
        payload->>'execution_id' as execution_id,
        CAST(payload->>'step_number' AS INTEGER) as step_number,
        payload->>'step_name' as step_name,
        payload->>'status' as status,
        source
    FROM events
    WHERE event_type = 'task_step'"#,
    // Typed view over artifact events
    r#"CREATE OR REPLACE VIEW artifacts AS
    SELECT
        timestamp,
        payload->>'artifact_uid' as artifact_uid,
        payload->>'namespace' as namespace,
        payload->>'name' as name,
        event_type,
        source
    FROM events
    WHERE event_type IN ('artifact_registered', 'artifact_transition')"#,
];

/// Memory tier view DDLs — query existing JSON files on disk via read_json_auto().
/// These scan disk on every query (acceptable for ad-hoc analytics, not hot-path).
/// Kept separate because they may fail if the directory structure doesn't exist yet.
const MEMORY_VIEW_DDLS: &[(&str, &str)] = &[
    (
        "episodes",
        r#"CREATE OR REPLACE VIEW episodes AS
        SELECT * FROM read_json_auto(
            'magician_data_v3/scopes/*/*/memory/agents/*/episodes/*.json',
            union_by_name = true,
            ignore_errors = true
        )"#,
    ),
    (
        "memory_tiers",
        r#"CREATE OR REPLACE VIEW memory_tiers AS
        SELECT * FROM read_json_auto(
            'magician_data_v3/scopes/*/*/memory/agents/*/tiers/*.json',
            union_by_name = true,
            ignore_errors = true
        )"#,
    ),
];

/// Create all convenience views on the given connection.
/// Safe to call on every startup — uses CREATE OR REPLACE.
pub fn create_views(conn: &Connection) -> Result<()> {
    for ddl in VIEW_DDLS {
        conn.execute_batch(ddl)
            .with_context(|| format!("creating view: {}", &ddl[..ddl.len().min(60)]))?;
    }

    // Memory tier views — best-effort, may fail if dirs don't exist yet.
    for (name, ddl) in MEMORY_VIEW_DDLS {
        if let Err(e) = conn.execute_batch(ddl) {
            debug!(target: "analytics", view = name, error = %e, "skipping memory view (dir may not exist yet)");
        }
    }

    let total = VIEW_DDLS.len() + MEMORY_VIEW_DDLS.len();
    debug!(target: "analytics", count = total, "created convenience views");
    Ok(())
}
