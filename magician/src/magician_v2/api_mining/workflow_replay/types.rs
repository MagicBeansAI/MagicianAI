//! Phase 3 replay types: `ReplayResult`, `StepReplayOutcome`, `ReplayError`,
//! `ReplayInputs`.

use crate::magician_v2::api_mining::workflow::BrowserFallbackStep;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResult {
    pub workflow_id: String,
    pub origin_key: String,
    pub steps: Vec<StepReplayOutcome>,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ReplayError>,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixedReplayResult {
    pub workflow_id: String,
    pub origin_key: String,
    pub steps: Vec<StepReplayOutcome>,
    /// `true` only when every non-skipped workflow step completed via API replay.
    pub success: bool,
    /// `true` when the engine stopped at a step that should continue on the
    /// browser rail. This is not treated as an API replay failure by itself.
    #[serde(default)]
    pub fallback_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<BrowserFallbackRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ReplayError>,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserFallbackRequest {
    pub step_id: String,
    pub step_index: usize,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_error: Option<ReplayError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserFallbackStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReplayOutcome {
    pub step_id: String,
    pub step_index: usize,
    #[serde(default)]
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_url: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub request_params: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_preview: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayError {
    UnknownCapability {
        step_id: String,
        capability_id: String,
    },
    ParamResolutionFailed {
        step_id: String,
        param: String,
        reason: String,
    },
    HttpFailure {
        step_id: String,
        status: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_preview: Option<String>,
    },
    VerificationFailed {
        step_id: String,
        status: u16,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_preview: Option<String>,
    },
    NetworkError {
        step_id: String,
        message: String,
    },
    BrowserOnlyStep {
        step_id: String,
    },
    JsonPathMiss {
        step_id: String,
        path: String,
        source_step: String,
    },
    AuthMissing {
        step_id: String,
        scheme: String,
    },
    WorkflowStale {
        workflow_id: String,
    },
    WorkflowTimeout {
        /// Wall-clock milliseconds elapsed when the budget was exceeded.
        elapsed_ms: u64,
        /// The budget caller supplied via `ReplayInputs.timeout_ms`.
        limit_ms: u64,
    },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCapability {
                step_id,
                capability_id,
            } => {
                write!(f, "step {step_id}: unknown capability {capability_id}")
            },
            Self::ParamResolutionFailed {
                step_id,
                param,
                reason,
            } => {
                write!(
                    f,
                    "step {step_id}: failed to resolve param {param}: {reason}"
                )
            },
            Self::HttpFailure {
                step_id, status, ..
            } => {
                write!(f, "step {step_id}: HTTP {status}")
            },
            Self::VerificationFailed {
                step_id,
                status,
                detail,
                ..
            } => {
                write!(
                    f,
                    "step {step_id}: replay verification failed at HTTP {status}: {detail}"
                )
            },
            Self::NetworkError { step_id, message } => {
                write!(f, "step {step_id}: network error: {message}")
            },
            Self::BrowserOnlyStep { step_id } => {
                write!(f, "step {step_id}: browser-only step (no capability)")
            },
            Self::JsonPathMiss {
                step_id,
                path,
                source_step,
            } => {
                write!(
                    f,
                    "step {step_id}: JSONPath {path} not found in step {source_step}"
                )
            },
            Self::AuthMissing { step_id, scheme } => {
                write!(
                    f,
                    "step {step_id}: missing session auth for scheme {scheme}"
                )
            },
            Self::WorkflowStale { workflow_id } => {
                write!(f, "workflow {workflow_id} is stale")
            },
            Self::WorkflowTimeout {
                elapsed_ms,
                limit_ms,
            } => {
                write!(
                    f,
                    "workflow replay exceeded {limit_ms}ms budget ({elapsed_ms}ms elapsed)"
                )
            },
        }
    }
}

impl std::error::Error for ReplayError {}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReplayInputs {
    #[serde(default)]
    pub user_inputs: HashMap<String, String>,
    /// Optional per-replay wall-clock budget in milliseconds. When set,
    /// the engine returns `ReplayError::WorkflowTimeout` if cumulative
    /// elapsed time crosses this threshold before all steps complete. No
    /// default — `None` means run to completion regardless of duration.
    /// Recommended for operator-initiated replays of workflows with
    /// unknown step counts; the per-step HTTP timeout still bounds
    /// individual stalls.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn replay_result_roundtrips_via_serde() {
        let result = ReplayResult {
            workflow_id: "wf_1".to_string(),
            origin_key: "example.com".to_string(),
            steps: vec![StepReplayOutcome {
                step_id: "step_0".to_string(),
                step_index: 0,
                skipped: false,
                skip_reason: None,
                capability_id: Some("cap_a".to_string()),
                replay_method: Some("GET".to_string()),
                replay_url: Some("https://example.com/api/items/1".to_string()),
                request_params: HashMap::from([("item_id".to_string(), "1".to_string())]),
                request_body: None,
                http_status: Some(200),
                response_body_preview: Some("{\"ok\":true}".to_string()),
                duration_ms: 42,
            }],
            success: true,
            failure: None,
            started_at_ms: 1_780_000_000_000,
            finished_at_ms: 1_780_000_000_500,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: ReplayResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.workflow_id, "wf_1");
        assert!(back.success);
    }

    #[test]
    fn mixed_replay_result_roundtrips_with_browser_fallback() {
        let result = MixedReplayResult {
            workflow_id: "wf_1".to_string(),
            origin_key: "example.com".to_string(),
            steps: vec![],
            success: false,
            fallback_required: true,
            fallback: Some(BrowserFallbackRequest {
                step_id: "step_2".to_string(),
                step_index: 2,
                reason: "browser-only step".to_string(),
                replay_error: Some(ReplayError::BrowserOnlyStep {
                    step_id: "step_2".to_string(),
                }),
                browser: Some(BrowserFallbackStep {
                    action: "click".to_string(),
                    arguments: serde_json::json!({"args":["#continue"]}),
                    description: Some("browser__click #continue".to_string()),
                }),
            }),
            failure: None,
            started_at_ms: 1,
            finished_at_ms: 2,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: MixedReplayResult = serde_json::from_str(&json).unwrap();
        assert!(back.fallback_required);
        assert_eq!(
            back.fallback
                .and_then(|fallback| fallback.browser)
                .map(|browser| browser.action),
            Some("click".to_string())
        );
    }

    #[test]
    fn replay_error_tagged_union_roundtrips() {
        let err = ReplayError::JsonPathMiss {
            step_id: "step_2".to_string(),
            path: "$.token".to_string(),
            source_step: "step_0".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: ReplayError = serde_json::from_str(&json).unwrap();
        match back {
            ReplayError::JsonPathMiss {
                step_id,
                path,
                source_step,
            } => {
                assert_eq!(step_id, "step_2");
                assert_eq!(path, "$.token");
                assert_eq!(source_step, "step_0");
            },
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn replay_error_display_includes_step_id() {
        let err = ReplayError::HttpFailure {
            step_id: "step_3".to_string(),
            status: 500,
            body_preview: None,
        };
        let s = format!("{err}");
        assert!(s.contains("step_3"));
        assert!(s.contains("500"));
    }

    #[test]
    fn replay_verification_error_display_includes_detail() {
        let err = ReplayError::VerificationFailed {
            step_id: "step_1".to_string(),
            status: 200,
            detail: "Schema structure mismatch".to_string(),
            body_preview: None,
        };
        let s = format!("{err}");
        assert!(s.contains("step_1"));
        assert!(s.contains("200"));
        assert!(s.contains("Schema structure mismatch"));
    }

    #[test]
    fn replay_inputs_defaults_to_empty_map() {
        let inputs = ReplayInputs::default();
        assert!(inputs.user_inputs.is_empty());
    }
}
