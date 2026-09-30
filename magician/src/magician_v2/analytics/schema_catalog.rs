use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::debug;

use super::pool::DuckDbPool;
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A snapshot of the analytics schema — discovered event types, counts, time
/// ranges, field names, and sample payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaCatalog {
    pub generated_at: DateTime<Utc>,
    pub event_types: Vec<EventTypeSchema>,
}

/// Schema summary for a single `event_type` value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTypeSchema {
    pub event_type: String,
    pub event_count: u64,
    pub earliest: DateTime<Utc>,
    pub latest: DateTime<Utc>,
    pub fields: Vec<String>,
    pub sample_payloads: Vec<JsonValue>,
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Query the `events` table to build a [`SchemaCatalog`].
///
/// This is a synchronous function — callers should run it on a blocking thread.
pub fn generate_schema_catalog(pool: &DuckDbPool) -> Result<SchemaCatalog> {
    let conn = pool.read_connection()?;

    // 1. Discover distinct event types with counts and time ranges.
    let mut stmt = conn
        .prepare(
            "SELECT event_type,
                    COUNT(*)::BIGINT AS cnt,
                    MIN(timestamp) AS earliest,
                    MAX(timestamp) AS latest
             FROM events
             GROUP BY event_type
             ORDER BY cnt DESC",
        )
        .context("preparing event type summary query")?;

    let type_rows: Vec<(String, i64, String, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .context("executing event type summary query")?
        .filter_map(|r| r.ok())
        .collect();

    let mut event_types = Vec::with_capacity(type_rows.len());

    for (event_type, count, earliest_str, latest_str) in &type_rows {
        // 2. Sample 3 recent payloads for this event type.
        let mut sample_stmt = conn
            .prepare(
                "SELECT payload
                 FROM events
                 WHERE event_type = ?
                 ORDER BY timestamp DESC
                 LIMIT 3",
            )
            .context("preparing sample query")?;

        let samples: Vec<JsonValue> = sample_stmt
            .query_map([event_type], |row| row.get::<_, String>(0))
            .context("executing sample query")?
            .filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();

        // 3. Extract top-level field names from the sampled payloads.
        let mut fields: Vec<String> = samples
            .iter()
            .filter_map(|v| v.as_object())
            .flat_map(|obj| obj.keys().cloned())
            .collect();
        fields.sort();
        fields.dedup();

        // Parse timestamps — fall back to now on parse errors.
        let earliest = parse_duckdb_timestamp(earliest_str).unwrap_or_else(|_| Utc::now());
        let latest = parse_duckdb_timestamp(latest_str).unwrap_or_else(|_| Utc::now());

        event_types.push(EventTypeSchema {
            event_type: event_type.clone(),
            event_count: *count as u64,
            earliest,
            latest,
            fields,
            sample_payloads: samples,
        });
    }

    debug!(
        target: "analytics",
        event_type_count = event_types.len(),
        "generated schema catalog"
    );

    Ok(SchemaCatalog {
        generated_at: Utc::now(),
        event_types,
    })
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Write the catalog to `<analytics_root>/schema_catalog.json`.
pub fn write_catalog_to_disk(catalog: &SchemaCatalog, analytics_root: &Path) -> Result<()> {
    std::fs::create_dir_all(analytics_root)
        .with_context(|| format!("creating catalog dir: {}", analytics_root.display()))?;

    let path = analytics_root.join("schema_catalog.json");
    let json = serde_json::to_string_pretty(catalog).context("serializing schema catalog")?;
    // The catalog is rewritten wholesale on every regeneration, so an in-place
    // write is a window in which the prompt-facing schema is half a document.
    write_bytes_durably_sync(&path, json.as_bytes())
        .with_context(|| format!("writing schema catalog to {}", path.display()))?;

    debug!(target: "analytics", path = %path.display(), "wrote schema catalog to disk");
    Ok(())
}

// ---------------------------------------------------------------------------
// LLM-friendly rendering
// ---------------------------------------------------------------------------

/// Render the catalog as Markdown suitable for prompt injection.
pub fn render_catalog_for_llm(catalog: &SchemaCatalog) -> String {
    let mut out = String::with_capacity(2048);

    out.push_str("# Analytics Schema Catalog\n\n");
    out.push_str(&format!(
        "Generated: {}\n\n",
        catalog.generated_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    out.push_str("The `events` table contains system events with columns: ");
    out.push_str("`id`, `timestamp`, `event_type`, `source`, `payload` (JSON).\n\n");

    if catalog.event_types.is_empty() {
        out.push_str("_No events recorded yet._\n");
        return out;
    }

    out.push_str("## Event Types\n\n");

    for et in &catalog.event_types {
        out.push_str(&format!("### `{}`\n\n", et.event_type));
        out.push_str(&format!("- **Count:** {}\n", et.event_count));
        out.push_str(&format!(
            "- **Range:** {} .. {}\n",
            et.earliest.format("%Y-%m-%d %H:%M:%S UTC"),
            et.latest.format("%Y-%m-%d %H:%M:%S UTC"),
        ));

        if !et.fields.is_empty() {
            out.push_str(&format!(
                "- **Payload fields:** `{}`\n",
                et.fields.join("`, `")
            ));
        }

        if !et.sample_payloads.is_empty() {
            out.push_str("\n<details><summary>Sample payloads</summary>\n\n```json\n");
            for sample in &et.sample_payloads {
                if let Ok(pretty) = serde_json::to_string_pretty(sample) {
                    out.push_str(&pretty);
                    out.push('\n');
                }
            }
            out.push_str("```\n</details>\n");
        }

        out.push('\n');
    }

    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Best-effort parse of a DuckDB TIMESTAMPTZ string into a `DateTime<Utc>`.
///
/// DuckDB may return timestamps in several formats depending on the column
/// type and output mode.  We try a few common patterns.
fn parse_duckdb_timestamp(s: &str) -> Result<DateTime<Utc>> {
    // Try ISO 8601 / RFC 3339 first.
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    // DuckDB often returns "YYYY-MM-DD HH:MM:SS.ffffff+00" or without offset.
    if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%#z") {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
        return Ok(naive.and_utc());
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Ok(naive.and_utc());
    }
    anyhow::bail!("unable to parse DuckDB timestamp: {}", s)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_catalog_to_disk_writes_into_scoped_analytics_root() {
        let tmp = TempDir::new().unwrap();
        let analytics_root = tmp.path().join("scopes/anonymous/default/analytics");
        let catalog = SchemaCatalog {
            generated_at: Utc::now(),
            event_types: vec![],
        };

        write_catalog_to_disk(&catalog, &analytics_root).unwrap();

        assert!(analytics_root.join("schema_catalog.json").exists());
        assert!(!analytics_root
            .join("analytics/schema_catalog.json")
            .exists());
    }

    #[test]
    fn write_catalog_to_disk_publishes_atomically_and_leaves_no_staging_file() {
        let tmp = TempDir::new().unwrap();
        let analytics_root = tmp.path().join("analytics");
        let catalog = SchemaCatalog {
            generated_at: Utc::now(),
            event_types: vec![EventTypeSchema {
                event_type: "log".to_string(),
                event_count: 1,
                earliest: Utc::now(),
                latest: Utc::now(),
                fields: vec!["level".to_string()],
                sample_payloads: vec![serde_json::json!({ "level": "info" })],
            }],
        };

        write_catalog_to_disk(&catalog, &analytics_root).unwrap();

        let mut names: Vec<String> = std::fs::read_dir(&analytics_root)
            .expect("analytics root listing")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["schema_catalog.json".to_string()],
            "the durable write must publish exactly one file, with no staging sibling"
        );

        let raw = std::fs::read_to_string(analytics_root.join("schema_catalog.json"))
            .expect("catalog body");
        let reloaded: SchemaCatalog =
            serde_json::from_str(&raw).expect("published catalog must parse");
        assert_eq!(reloaded.event_types.len(), 1);
    }
}
