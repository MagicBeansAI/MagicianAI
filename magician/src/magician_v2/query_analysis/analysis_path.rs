//! Which path produces a planning turn's analysis, and the shadow record.
//!
//! `legacy` runs the unified query-analysis LLM call on every turn (today).
//! `new` takes intent, task clarity and the complexity bucket from the gated
//! `query_intent` engine answers and entity extraction (with the 0.7
//! clarification signal) from the slot extractor, so the unified call runs
//! only as a fallback. `shadow` keeps the legacy result and also runs the new
//! path off the hot path, appending one comparison record per stage to the
//! scope's `planning/analysis_path_shadow.jsonl`.
//!
//! Set from `planning.analysis_path` in `magician-config.yaml` at boot and on
//! config reload. Design and field map:
//! `docs/archive/plans/2026-09-30-planning-decisions-on-decision-engine.md` (Phase C).

use std::sync::atomic::{AtomicU8, Ordering};

use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info, warn};

pub use crate::config::PlanningAnalysisPath as AnalysisPath;
use crate::magician_v2::slot_graph::extraction::LlmCallTelemetry;

static CURRENT: AtomicU8 = AtomicU8::new(0);

fn encode(path: AnalysisPath) -> u8 {
    match path {
        AnalysisPath::Legacy => 0,
        AnalysisPath::Shadow => 1,
        AnalysisPath::New => 2,
    }
}

fn decode(raw: u8) -> AnalysisPath {
    match raw {
        1 => AnalysisPath::Shadow,
        2 => AnalysisPath::New,
        _ => AnalysisPath::Legacy,
    }
}

/// Install the configured path (boot and config reload).
pub fn install(path: AnalysisPath) {
    let previous = decode(CURRENT.swap(encode(path), Ordering::Relaxed));
    if previous != path {
        info!(?previous, current = ?path, "[PLANNING-ANALYSIS-PATH] analysis path changed");
    }
}

/// The path planning turns use now. Legacy until installed.
pub fn current() -> AnalysisPath {
    decode(CURRENT.load(Ordering::Relaxed))
}

/// The clarification threshold the elicitation stage applies to
/// `extraction_confidence` (`ElicitationConfig::default().confidence_gate`).
pub const EXTRACTION_GATE: f32 = 0.7;

/// One model call's cost, from the router's telemetry.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CallCost {
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl CallCost {
    pub fn from_telemetry(latency_ms: u64, telemetry: Option<&LlmCallTelemetry>) -> Self {
        let Some(telemetry) = telemetry else {
            return Self {
                latency_ms,
                ..Self::default()
            };
        };
        Self {
            latency_ms,
            model: Some(telemetry.model.clone()),
            input_tokens: telemetry.usage_reported.then_some(telemetry.input_tokens),
            output_tokens: telemetry.usage_reported.then_some(telemetry.output_tokens),
            cost_usd: telemetry.usage_reported.then_some(telemetry.cost_usd),
        }
    }
}

/// Where the shadow records for a scope go.
pub fn shadow_log_path(principal: &str, workspace: &str) -> std::path::PathBuf {
    crate::magician_v2::process_storage::workspace()
        .scope_root(principal, workspace)
        .join("planning")
        .join("analysis_path_shadow.jsonl")
}

/// Append one comparison record. Best effort: a failed write is logged and
/// never touches the planning turn.
pub async fn record_shadow(principal: &str, workspace: &str, record: Value) {
    let principal = principal.trim();
    let workspace = workspace.trim();
    if principal.is_empty() || workspace.is_empty() {
        return;
    }
    debug!(
        target: "planning_analysis_shadow",
        record = %record,
        "[PLANNING-ANALYSIS-PATH] shadow record"
    );
    let path = shadow_log_path(principal, workspace);
    if let Err(error) = append_jsonl(&path, &record).await {
        warn!(%error, path = %path.display(), "[PLANNING-ANALYSIS-PATH] shadow record not written");
    }
}

/// Append one JSON line, creating the file and its directory.
pub async fn append_jsonl(path: &std::path::Path, record: &Value) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(&line).await?;
    file.flush().await
}

/// The first characters of the message, for reading a record without the
/// thread.
pub fn query_head(query: &str) -> String {
    query.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_round_trips_and_defaults_to_legacy() {
        for path in [
            AnalysisPath::Legacy,
            AnalysisPath::Shadow,
            AnalysisPath::New,
        ] {
            assert_eq!(decode(encode(path)), path);
        }
        assert_eq!(decode(7), AnalysisPath::Legacy);
        assert_eq!(AnalysisPath::default(), AnalysisPath::Legacy);
    }

    #[test]
    fn config_spelling_is_snake_case() {
        let settings: crate::config::PlanningSettings =
            serde_yaml::from_str("analysis_path: shadow").expect("parses");
        assert_eq!(settings.analysis_path, AnalysisPath::Shadow);
        let settings: crate::config::PlanningSettings =
            serde_yaml::from_str("{}").expect("empty parses");
        assert_eq!(settings.analysis_path, AnalysisPath::Legacy);
        assert!(serde_yaml::from_str::<crate::config::PlanningSettings>("bogus: 1").is_err());
    }

    #[tokio::test]
    async fn shadow_records_append_one_line_each() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .join("planning")
            .join("analysis_path_shadow.jsonl");
        append_jsonl(&path, &serde_json::json!({"stage": "analysis", "n": 1}))
            .await
            .expect("first append");
        append_jsonl(&path, &serde_json::json!({"stage": "extraction", "n": 2}))
            .await
            .expect("second append");
        let text = std::fs::read_to_string(&path).expect("read back");
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line is JSON"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["stage"], "extraction");
    }

    #[test]
    fn shadow_log_lives_in_the_scope() {
        let path = shadow_log_path("anonymous", "default");
        assert!(path.ends_with("anonymous/default/planning/analysis_path_shadow.jsonl"));
    }

    #[test]
    fn call_cost_keeps_unreported_usage_absent() {
        let cost = CallCost::from_telemetry(12, None);
        assert_eq!(cost.latency_ms, 12);
        assert!(cost.input_tokens.is_none());
    }
}
