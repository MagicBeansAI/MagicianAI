//! In-memory recorder for one execution's worth of sequence steps.
//!
//! Owned by the executor for the duration of one run. After each action the
//! executor calls `record_api_step` or `record_browser_step`. On execution
//! completion the executor calls `finalize()` and persists the resulting
//! `CapabilitySequence` via `SequenceStore`.

use crate::magician_v2::api_mining::sequence::{CapabilitySequence, ExecutionPath, SequenceStep};
use std::collections::HashMap;
use ulid::Ulid;

pub struct SequenceRecorder {
    sequence_id: String,
    task_id: String,
    execution_id: String,
    origin_key: String,
    captured_at_ms: i64,
    steps: Vec<SequenceStep>,
}

impl SequenceRecorder {
    pub fn start(task_id: &str, execution_id: &str, origin_key: &str, captured_at_ms: i64) -> Self {
        Self {
            sequence_id: format!("seq_{}", Ulid::new()),
            task_id: task_id.to_string(),
            execution_id: execution_id.to_string(),
            origin_key: origin_key.to_string(),
            captured_at_ms,
            steps: Vec::new(),
        }
    }

    pub fn sequence_id(&self) -> &str {
        &self.sequence_id
    }

    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// `true` if any recorded step ran in the browser (executed_via=Browser)
    /// AND has no correlated capability. Phase 3 uses this signal to decide
    /// whether full browserless replay is possible.
    pub fn has_browser_only_steps(&self) -> bool {
        self.steps
            .iter()
            .any(|s| s.executed_via == ExecutionPath::Browser && s.capability_id.is_none())
    }

    /// Record an action that was API-replayed (router returned `Replay`).
    #[allow(clippy::too_many_arguments)]
    pub fn record_api_step(
        &mut self,
        capability_id: Option<String>,
        origin: &str,
        concrete_url: &str,
        method: &str,
        request_params: HashMap<String, String>,
        request_body: Option<&str>,
        response_status: Option<u16>,
        response_body: Option<&str>,
        action_binding_id: Option<String>,
        timestamp_ms: i64,
        duration_ms: u64,
    ) {
        let step = SequenceStep {
            step_index: self.steps.len(),
            capability_id,
            origin: origin.to_string(),
            concrete_url: concrete_url.to_string(),
            method: method.to_string(),
            request_params,
            request_body: request_body.map(str::to_string),
            response_status,
            response_body: SequenceStep::truncate_response_body(response_body),
            action_binding_id,
            browser_action_desc: None,
            browser_action: None,
            browser_arguments: None,
            executed_via: ExecutionPath::ApiReplay,
            timestamp_ms,
            duration_ms,
        };
        self.steps.push(step);
    }

    /// Record an action that ran in the browser (router PassThrough'd or the
    /// action was non-API-replayable).
    #[allow(clippy::too_many_arguments)]
    pub fn record_browser_step(
        &mut self,
        capability_id: Option<String>,
        origin: &str,
        concrete_url: Option<&str>,
        method: Option<&str>,
        response_status: Option<u16>,
        response_body: Option<&str>,
        browser_action_desc: &str,
        browser_action: Option<&str>,
        browser_arguments: Option<&serde_json::Value>,
        timestamp_ms: i64,
        duration_ms: u64,
    ) {
        let step = SequenceStep {
            step_index: self.steps.len(),
            capability_id,
            origin: origin.to_string(),
            concrete_url: concrete_url.unwrap_or("").to_string(),
            method: method.unwrap_or("").to_string(),
            request_params: HashMap::new(),
            request_body: None,
            response_status,
            response_body: SequenceStep::truncate_response_body(response_body),
            action_binding_id: None,
            browser_action_desc: Some(browser_action_desc.to_string()),
            browser_action: browser_action.map(str::to_string),
            browser_arguments: browser_arguments.cloned(),
            executed_via: ExecutionPath::Browser,
            timestamp_ms,
            duration_ms,
        };
        self.steps.push(step);
    }

    /// Record a browser-executed action that is now known to map to a
    /// replayable API capability. The execution path stays `Browser` because
    /// this invocation did not use the router, but the capability metadata lets
    /// workflow compilation treat the step as browserless on future runs.
    #[allow(clippy::too_many_arguments)]
    pub fn record_browser_capability_step(
        &mut self,
        capability_id: String,
        origin: &str,
        concrete_url: &str,
        method: &str,
        request_params: HashMap<String, String>,
        response_status: Option<u16>,
        response_body: Option<&str>,
        browser_action_desc: &str,
        browser_action: Option<&str>,
        browser_arguments: Option<&serde_json::Value>,
        timestamp_ms: i64,
        duration_ms: u64,
    ) {
        let step = SequenceStep {
            step_index: self.steps.len(),
            capability_id: Some(capability_id),
            origin: origin.to_string(),
            concrete_url: concrete_url.to_string(),
            method: method.to_string(),
            request_params,
            request_body: None,
            response_status,
            response_body: SequenceStep::truncate_response_body(response_body),
            action_binding_id: None,
            browser_action_desc: Some(browser_action_desc.to_string()),
            browser_action: browser_action.map(str::to_string),
            browser_arguments: browser_arguments.cloned(),
            executed_via: ExecutionPath::Browser,
            timestamp_ms,
            duration_ms,
        };
        self.steps.push(step);
    }

    pub fn finalize(self) -> CapabilitySequence {
        CapabilitySequence {
            id: self.sequence_id,
            task_id: self.task_id,
            execution_id: self.execution_id,
            origin_key: self.origin_key,
            steps: self.steps,
            captured_at_ms: self.captured_at_ms,
            finalized: true,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn recorder_records_api_replayed_step() {
        let mut rec = SequenceRecorder::start("task_x", "exec_x", "example.com", 1_780_000_000_000);
        rec.record_api_step(
            Some("cap_a".to_string()),
            "https://example.com",
            "https://example.com/api/x",
            "GET",
            HashMap::from([("id".to_string(), "1".to_string())]),
            None,
            Some(200),
            Some("{\"ok\":true}"),
            Some("ab_1".to_string()),
            1_780_000_000_500,
            87,
        );
        let seq = rec.finalize();
        assert_eq!(seq.steps.len(), 1);
        assert_eq!(seq.steps[0].executed_via, ExecutionPath::ApiReplay);
        assert_eq!(seq.steps[0].step_index, 0);
        assert_eq!(seq.steps[0].response_body.as_deref(), Some("{\"ok\":true}"));
        assert!(seq.finalized);
    }

    #[test]
    fn recorder_records_browser_only_step() {
        let mut rec = SequenceRecorder::start("task_x", "exec_x", "example.com", 1_780_000_000_000);
        rec.record_browser_step(
            None,
            "https://example.com",
            None,
            None,
            None,
            None,
            "Click .submit-btn",
            Some("click"),
            Some(&serde_json::json!({"args":[".submit-btn"]})),
            1_780_000_000_500,
            12,
        );
        let seq = rec.finalize();
        assert_eq!(seq.steps.len(), 1);
        assert_eq!(seq.steps[0].executed_via, ExecutionPath::Browser);
        assert!(seq.steps[0].capability_id.is_none());
        assert_eq!(
            seq.steps[0].browser_action_desc.as_deref(),
            Some("Click .submit-btn")
        );
        assert_eq!(seq.steps[0].browser_action.as_deref(), Some("click"));
        assert_eq!(
            seq.steps[0].browser_arguments.as_ref(),
            Some(&serde_json::json!({"args":[".submit-btn"]}))
        );
    }

    #[test]
    fn recorder_records_browser_captured_capability_step() {
        let mut rec = SequenceRecorder::start("task_x", "exec_x", "example.com", 1_780_000_000_000);
        rec.record_browser_capability_step(
            "cap_json".to_string(),
            "https://example.com",
            "https://example.com/api/items/42",
            "GET",
            HashMap::from([("item_id".to_string(), "42".to_string())]),
            Some(200),
            Some(r#"{"id":42}"#),
            "browser__get",
            Some("get"),
            Some(&serde_json::json!({"args":["https://example.com/api/items/42"]})),
            1_780_000_000_500,
            12,
        );
        let seq = rec.finalize();
        assert_eq!(seq.steps.len(), 1);
        assert_eq!(seq.steps[0].executed_via, ExecutionPath::Browser);
        assert_eq!(seq.steps[0].capability_id.as_deref(), Some("cap_json"));
        assert_eq!(seq.steps[0].method, "GET");
        assert_eq!(
            seq.steps[0]
                .request_params
                .get("item_id")
                .map(String::as_str),
            Some("42")
        );
        assert!(!seq.steps[0].request_params.is_empty());
    }

    #[test]
    fn recorder_assigns_monotonic_step_indices() {
        let mut rec = SequenceRecorder::start("task_x", "exec_x", "example.com", 1_780_000_000_000);
        rec.record_browser_step(
            None,
            "https://example.com",
            None,
            None,
            None,
            None,
            "a",
            Some("click"),
            Some(&serde_json::json!({"args":["#a"]})),
            1,
            0,
        );
        rec.record_browser_step(
            None,
            "https://example.com",
            None,
            None,
            None,
            None,
            "b",
            Some("click"),
            Some(&serde_json::json!({"args":["#b"]})),
            2,
            0,
        );
        rec.record_browser_step(
            None,
            "https://example.com",
            None,
            None,
            None,
            None,
            "c",
            Some("click"),
            Some(&serde_json::json!({"args":["#c"]})),
            3,
            0,
        );
        let seq = rec.finalize();
        let indices: Vec<usize> = seq.steps.iter().map(|s| s.step_index).collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[test]
    fn recorder_finalize_truncates_large_response_bodies() {
        let mut rec = SequenceRecorder::start("task_x", "exec_x", "example.com", 1_780_000_000_000);
        let big = "x".repeat(8 * 1024);
        rec.record_api_step(
            Some("cap_a".to_string()),
            "https://example.com",
            "https://example.com/api/x",
            "GET",
            HashMap::new(),
            None,
            Some(200),
            Some(&big),
            None,
            1_780_000_000_500,
            87,
        );
        let seq = rec.finalize();
        let body = seq.steps[0].response_body.as_ref().expect("body");
        assert!(body.len() < 6 * 1024);
        assert!(body.ends_with("…"));
    }

    #[test]
    fn recorder_generates_unique_sequence_id() {
        let r1 = SequenceRecorder::start("t", "e", "o", 1);
        let r2 = SequenceRecorder::start("t", "e", "o", 1);
        assert_ne!(r1.sequence_id(), r2.sequence_id());
        assert!(r1.sequence_id().starts_with("seq_"));
    }

    #[test]
    fn recorder_caps_oversize_response_body_at_executor_boundary() {
        // Regression check: pinned against the executor-side path that
        // calls record_api_step with whatever body_text the replay
        // response yielded. If anyone changes RESPONSE_BODY_CAP_BYTES,
        // this test catches the drift.
        let mut rec = SequenceRecorder::start("t", "e", "example.com", 1);
        let body = "y".repeat(10 * 1024);
        rec.record_api_step(
            Some("cap".to_string()),
            "https://example.com",
            "https://example.com/api",
            "GET",
            HashMap::new(),
            None,
            Some(200),
            Some(&body),
            None,
            1,
            0,
        );
        let seq = rec.finalize();
        let stored = seq.steps[0].response_body.as_ref().unwrap();
        assert!(stored.len() < 6 * 1024);
        assert!(stored.ends_with("…"));
    }

    #[test]
    fn recorder_detects_browser_only_steps() {
        let mut rec = SequenceRecorder::start("t", "e", "o", 1);
        rec.record_browser_step(
            None,
            "o",
            None,
            None,
            None,
            None,
            "click",
            Some("click"),
            Some(&serde_json::json!({"args":["#x"]})),
            1,
            0,
        );
        assert!(rec.has_browser_only_steps());

        let mut rec2 = SequenceRecorder::start("t", "e", "o", 1);
        rec2.record_browser_step(
            Some("cap".to_string()),
            "o",
            None,
            None,
            None,
            None,
            "click",
            Some("click"),
            Some(&serde_json::json!({"args":["#x"]})),
            1,
            0,
        );
        // Browser step WITH a capability is mixed-mode, not browser-only.
        assert!(!rec2.has_browser_only_steps());
    }
}
