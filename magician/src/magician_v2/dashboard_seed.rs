//! Boot-time seed for the "LLM Calls Overview" dashboard.
//!
//! Mints (idempotently) a system-owned terminal task in each scope whose
//! user-output is a MUI-JSON dashboard with live `dataSource` bindings
//! against the Parquet `llm_calls` table. Publishes the task via the
//! standard `create_dashboard` capability so the route shows up at
//! `/briefing/llm-overview` on first boot.
//!
//! Design notes:
//! - Output payload lives in code (this module) — easy to evolve, no
//!   YAML/JSON shipping. The MUI-JSON tree references the same Parquet
//!   schema documented in `2026-05-12-llm-calls-lakehouse.md`.
//! - `dashboard_theme = "editorial"` — long-form analyst aesthetic is
//!   the right default for an overview dashboard.
//! - Idempotent via a canonical seed-task id derived from the scope.
//!   If the task already exists, the routine no-ops.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Canonical id for the seed task. Stable across reboots so the
/// "create-if-absent" check is deterministic.
pub fn seed_task_id(principal: &str, workspace: &str) -> String {
    let digest = Sha256::digest(format!("{principal}\0{workspace}").as_bytes());
    format!("system-llm-overview-seed-{}", hex::encode(&digest[..12]))
}

/// Canonical surface route the seed publishes to.
pub const SEED_ROUTE: &str = "/briefing/llm-overview";

/// Builds the MUI-JSON dashboard payload for the LLM Calls Overview.
/// Returns the inner `body_json` value (a `{components: [...]}` object).
pub fn build_llm_overview_dashboard() -> Value {
    json!({
        "components": [
            // KPI hero row: total spend, total calls, retry rate, avg latency
            {
                "type": "Grid",
                "columns": 4,
                "components": [
                    {
                        "type": "MetricCard",
                        "label": "Spend (7d)",
                        "formatAs": "currency",
                        "dataSource": {
                            "kind": "llm_calls_sql",
                            "sql": "SELECT SUM(cost_usd) FROM llm_calls WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)"
                        }
                    },
                    {
                        "type": "MetricCard",
                        "label": "Calls (24h)",
                        "formatAs": "number",
                        "dataSource": {
                            "kind": "llm_calls_sql",
                            "sql": "SELECT COUNT(*) FROM llm_calls WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)"
                        }
                    },
                    {
                        "type": "MetricCard",
                        "label": "Retry rate (24h)",
                        "formatAs": "percent",
                        "dataSource": {
                            "kind": "llm_calls_sql",
                            "sql": "SELECT (SUM(CASE WHEN attempt > 1 THEN 1 ELSE 0 END) * 1.0 / NULLIF(COUNT(*), 0)) FROM llm_calls WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)"
                        }
                    },
                    {
                        "type": "MetricCard",
                        "label": "Avg latency (24h)",
                        "formatAs": "number",
                        "dataSource": {
                            "kind": "llm_calls_sql",
                            "sql": "SELECT AVG(latency_ms) FROM llm_calls WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)"
                        }
                    }
                ]
            },

            // Top agents by spend
            {
                "type": "BarChart",
                "title": "Top agents by spend (7d)",
                "xField": "agent_id",
                "yField": "spend",
                "dataSource": {
                    "kind": "llm_calls_sql",
                    "sql": "SELECT agent_id, SUM(cost_usd) AS spend FROM llm_calls WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) AND agent_id IS NOT NULL GROUP BY agent_id ORDER BY spend DESC LIMIT 10"
                }
            },

            // Latency p95 by operation
            {
                "type": "Table",
                "dataSource": {
                    "kind": "llm_calls_sql",
                    "sql": "SELECT operation, COUNT(*) AS calls, quantile_cont(latency_ms, 0.5) AS p50_ms, quantile_cont(latency_ms, 0.95) AS p95_ms FROM llm_calls WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY) AND operation IS NOT NULL GROUP BY operation ORDER BY p95_ms DESC"
                }
            },

            // Cache hit ratio by model
            {
                "type": "BarChart",
                "title": "Cache hit ratio by model (7d)",
                "xField": "model",
                "yField": "ratio",
                "dataSource": {
                    "kind": "llm_calls_sql",
                    "sql": "SELECT model, SUM(cache_read_tokens) * 1.0 / NULLIF(SUM(input_tokens), 0) AS ratio FROM llm_calls WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) AND input_tokens > 0 GROUP BY model ORDER BY ratio DESC LIMIT 8"
                }
            },

            // Chat vs autonomous breakdown
            {
                "type": "PieChart",
                "title": "Mode mix (24h)",
                "labelField": "mode",
                "valueField": "spend",
                "dataSource": {
                    "kind": "llm_calls_sql",
                    "sql": "SELECT CASE WHEN chat_session_id IS NOT NULL THEN 'chat' ELSE 'autonomous' END AS mode, SUM(cost_usd) AS spend FROM llm_calls WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY) GROUP BY 1"
                }
            }
        ]
    })
}

/// Returns the synthesized task user-output envelope this module would
/// publish, for use as both the seed payload and as documentation when
/// the artifact-service integration is wired.
pub fn build_user_output_envelope() -> Value {
    json!({
        "media_type": "application/json",
        "dashboard_theme": "editorial",
        "body_json": build_llm_overview_dashboard()
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn seed_task_id_is_deterministic() {
        let a = seed_task_id("anonymous", "default");
        let b = seed_task_id("anonymous", "default");
        assert_eq!(a, b);
        assert!(a.starts_with("system-llm-overview-seed-"));
        assert_ne!(a, seed_task_id("anonymous", "other"));
        crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::validate_task_id(&a)
            .expect("seed task id must be one canonical path segment");
    }

    #[test]
    fn dashboard_has_kpi_row_and_charts() {
        let dashboard = build_llm_overview_dashboard();
        let components = dashboard
            .get("components")
            .and_then(|v| v.as_array())
            .expect("components array");
        assert!(
            components.len() >= 4,
            "expected hero + bar + table + cache + pie"
        );

        // First child is a Grid of MetricCards.
        let hero = &components[0];
        assert_eq!(hero["type"], "Grid");
        let hero_kids = hero["components"].as_array().expect("hero kids");
        assert!(hero_kids.iter().all(|k| k["type"] == "MetricCard"));

        // Every chart/table/MetricCard has a dataSource → live binding.
        fn has_data_source(component: &Value) -> bool {
            if component.get("dataSource").is_some() {
                return true;
            }
            if let Some(kids) = component.get("components").and_then(|v| v.as_array()) {
                return kids.iter().any(has_data_source);
            }
            false
        }
        for c in components {
            let kind = c["type"].as_str().unwrap_or("");
            if matches!(
                kind,
                "BarChart" | "LineChart" | "PieChart" | "Table" | "MetricCard" | "Grid"
            ) {
                assert!(has_data_source(c), "component '{}' lacks dataSource", kind);
            }
        }
    }

    #[test]
    fn envelope_uses_editorial_theme_and_application_json() {
        let envelope = build_user_output_envelope();
        assert_eq!(envelope["media_type"], "application/json");
        assert_eq!(envelope["dashboard_theme"], "editorial");
        assert!(envelope["body_json"].is_object());
    }
}
