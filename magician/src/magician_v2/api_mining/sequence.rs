//! Workflow-replay Phase 1: ordered capture of one automation run.
//!
//! A `CapabilitySequence` is one execution's worth of `SequenceStep`s in the
//! order they were performed. Each step references the action's correlated
//! capability (when one exists) plus the concrete request/response and the
//! action binding used. Compilation into a `WorkflowGraph` is Phase 2; this
//! module only owns the capture model.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const RESPONSE_BODY_CAP_BYTES: usize = 4 * 1024;

/// One automation run's ordered capability sequence, persisted under
/// `<scope>/api_mining/<origin_key>/sequences/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySequence {
    pub id: String,
    pub task_id: String,
    pub execution_id: String,
    pub origin_key: String,
    pub steps: Vec<SequenceStep>,
    pub captured_at_ms: i64,
    /// `false` between `start()` and `finalize()`; `true` after `finalize()`.
    #[serde(default)]
    pub finalized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceStep {
    pub step_index: usize,
    /// `None` for browser-only steps (no correlated API capability).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    pub origin: String,
    /// Concrete URL the action targeted (after template substitution).
    /// Empty string for purely browser-only steps that didn't fire an XHR.
    pub concrete_url: String,
    /// HTTP method ("GET", "POST", ...). Empty string for browser-only steps.
    pub method: String,
    #[serde(default)]
    pub request_params: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_status: Option<u16>,
    /// Truncated to RESPONSE_BODY_CAP_BYTES with an ellipsis marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// `ActionBinding::id` if known (Phase 2 uses this for cross-step matching).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_binding_id: Option<String>,
    /// Human-readable description for browser-only steps that had no capability
    /// (e.g., "Click .submit-btn", "Scroll to bottom").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_action_desc: Option<String>,
    /// Browser primitive action name that produced this step, when known.
    /// Preserved so mixed workflow replay can fall back to an executable browser
    /// action instead of a human-only description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_action: Option<String>,
    /// Original browser primitive arguments. Kept opaque because the browser
    /// dispatcher owns the schema for each action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_arguments: Option<serde_json::Value>,
    pub executed_via: ExecutionPath,
    pub timestamp_ms: i64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPath {
    /// The action ran in the browser.
    Browser,
    /// The router replayed the action via direct HTTP.
    ApiReplay,
}

impl SequenceStep {
    /// Cap `response_body` at RESPONSE_BODY_CAP_BYTES; append `…` on truncate.
    /// Operates on byte length, not char count — DuckDB / JSON consumers care
    /// about disk + parse cost.
    pub fn truncate_response_body(body: Option<&str>) -> Option<String> {
        let body = body?;
        if body.len() <= RESPONSE_BODY_CAP_BYTES {
            return Some(body.to_string());
        }
        // Find a UTF-8 char boundary at or before the cap.
        let mut cap = RESPONSE_BODY_CAP_BYTES;
        while cap > 0 && !body.is_char_boundary(cap) {
            cap -= 1;
        }
        let mut truncated = body[..cap].to_string();
        truncated.push('…');
        Some(truncated)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn sequence_step_roundtrips_via_serde() {
        let step = SequenceStep {
            step_index: 0,
            capability_id: Some("cap_abc".to_string()),
            origin: "https://example.com".to_string(),
            concrete_url: "https://example.com/api/widgets/42".to_string(),
            method: "GET".to_string(),
            request_params: [("id".to_string(), "42".to_string())].into(),
            request_body: None,
            response_status: Some(200),
            response_body: Some("{\"id\":42}".to_string()),
            action_binding_id: Some("ab_xyz".to_string()),
            browser_action_desc: None,
            browser_action: None,
            browser_arguments: None,
            executed_via: ExecutionPath::ApiReplay,
            timestamp_ms: 1_780_000_000_000,
            duration_ms: 87,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: SequenceStep = serde_json::from_str(&json).unwrap();
        assert_eq!(back.capability_id.as_deref(), Some("cap_abc"));
        assert_eq!(back.executed_via, ExecutionPath::ApiReplay);
        assert_eq!(back.response_body.as_deref(), Some("{\"id\":42}"));
    }

    #[test]
    fn capability_sequence_browser_only_step_skips_capability_id() {
        let step = SequenceStep {
            step_index: 0,
            capability_id: None,
            origin: "https://example.com".to_string(),
            concrete_url: String::new(),
            method: String::new(),
            request_params: Default::default(),
            request_body: None,
            response_status: None,
            response_body: None,
            action_binding_id: None,
            browser_action_desc: Some("Click .submit-btn".to_string()),
            browser_action: Some("click".to_string()),
            browser_arguments: Some(serde_json::json!({"args":["#submit"]})),
            executed_via: ExecutionPath::Browser,
            timestamp_ms: 1_780_000_000_000,
            duration_ms: 5,
        };
        let sequence = CapabilitySequence {
            id: "seq_test".to_string(),
            task_id: "task_xyz".to_string(),
            execution_id: "exec_abc".to_string(),
            origin_key: "example.com".to_string(),
            steps: vec![step],
            captured_at_ms: 1_780_000_000_000,
            finalized: true,
        };
        let json = serde_json::to_string(&sequence).unwrap();
        let back: CapabilitySequence = serde_json::from_str(&json).unwrap();
        assert_eq!(back.steps.len(), 1);
        assert!(back.steps[0].capability_id.is_none());
        assert_eq!(
            back.steps[0].browser_action_desc.as_deref(),
            Some("Click .submit-btn")
        );
        assert_eq!(back.steps[0].browser_action.as_deref(), Some("click"));
        assert_eq!(
            back.steps[0]
                .browser_arguments
                .as_ref()
                .and_then(|value| value.get("args"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn sequence_step_truncates_oversized_response_body() {
        let big = "x".repeat(8 * 1024);
        let truncated = SequenceStep::truncate_response_body(Some(&big));
        assert!(truncated.is_some());
        let body = truncated.unwrap();
        assert!(body.len() <= 4 * 1024 + 16);
        assert!(body.ends_with("…"));
    }

    #[test]
    fn sequence_step_passes_through_small_response_body() {
        let small = r#"{"id":42}"#.to_string();
        let unchanged = SequenceStep::truncate_response_body(Some(&small));
        assert_eq!(unchanged.as_deref(), Some(small.as_str()));
    }
}
