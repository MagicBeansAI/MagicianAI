//! Phase 3 single-step executor. Wraps `ApiRunner::replay_with_reqwest` so
//! the engine can fire one HTTP call at a time and collect the response for
//! downstream data-flow lookups.

use crate::magician_v2::api_mining::replay::{ApiRunner, ReplayRequest};
use crate::magician_v2::api_mining::types::SessionContext;
use crate::magician_v2::api_mining::workflow_replay::types::ReplayError;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const BODY_PREVIEW_CAP_BYTES: usize = 4 * 1024;

pub struct StepExecutor {
    runner: Arc<Mutex<ApiRunner>>,
    /// The dispatch attempt this replay belongs to; see `with_effect_id`.
    effect_id: Option<String>,
}

pub struct StepExecutionResult {
    pub http_status: u16,
    pub response_body_text: String,
    pub response_body_json: Option<Value>,
    pub duration_ms: u64,
    pub replay_request: Option<ReplayRequest>,
    pub request_params: HashMap<String, String>,
}

impl StepExecutor {
    pub fn new(runner: Arc<Mutex<ApiRunner>>) -> Self {
        Self {
            runner,
            effect_id: None,
        }
    }

    /// Attach the dispatch attempt this replay belongs to.
    ///
    /// A workflow replay is the one place a single model tool call becomes
    /// SEVERAL real HTTP requests: one `browser__click` can take over into a
    /// recorded workflow and run every step of it live. One id for the whole
    /// replay would therefore not identify anything — each step is its own
    /// effect and needs its own key.
    ///
    /// The executor is constructed per replay, so it can hold the attempt
    /// identity and compose a per-step key without widening the engine's
    /// `StepExecutor::execute` contract or threading it through every layer
    /// between.
    pub fn with_effect_id(mut self, effect_id: Option<String>) -> Self {
        self.effect_id = effect_id;
        self
    }

    /// Per-step effect key: `{effect_id}:step:{step_id}`.
    ///
    /// `None` when the replay has no attributable dispatch behind it — which
    /// means "not attributable", never "safe to repeat".
    pub fn step_effect_id(&self, step_id: &str) -> Option<String> {
        self.effect_id
            .as_ref()
            .map(|effect_id| format!("{effect_id}:step:{step_id}"))
    }

    /// Execute one workflow step via `ApiRunner::replay_with_reqwest`.
    ///
    /// Uses `tokio::sync::Mutex` so the MutexGuard stays Send across the
    /// `.await` boundary — `std::sync::Mutex` would have made the future
    /// non-Send and silently locked the engine into actix's `?Send`
    /// runtime. Since the engine calls execute() sequentially per step,
    /// the async lock is uncontended in practice.
    pub async fn execute(
        &self,
        step_id: &str,
        origin_key: &str,
        capability_id: &str,
        params: &HashMap<String, String>,
        session_ctx: &SessionContext,
    ) -> Result<StepExecutionResult, ReplayError> {
        let started = std::time::Instant::now();

        let (replay_request, inner_result) = {
            let mut runner = self.runner.lock().await;
            let replay_request = runner
                .prepare_replay(origin_key, capability_id, params, session_ctx)
                .map_err(|err| {
                    if is_capability_not_found(&err) {
                        ReplayError::UnknownCapability {
                            step_id: step_id.to_string(),
                            capability_id: capability_id.to_string(),
                        }
                    } else {
                        ReplayError::NetworkError {
                            step_id: step_id.to_string(),
                            message: err,
                        }
                    }
                })?
                .map(|(request, _)| request);
            let inner_result = runner
                .replay_with_reqwest(
                    origin_key,
                    capability_id,
                    params,
                    session_ctx,
                    None,
                    // Per STEP, not per replay: one model tool call can take
                    // over into a recorded workflow and run every step of it
                    // live, so each step is its own effect.
                    self.step_effect_id(step_id).as_deref(),
                )
                .await;
            (replay_request, inner_result)
        };

        let result = inner_result.map_err(|err| {
            // CapabilityStore::load returns a stringified error for missing
            // capability files. Detect it and surface as the specific
            // ReplayError::UnknownCapability so operators can distinguish
            // "registry doesn't know this capability" from "network failed".
            if is_capability_not_found(&err) {
                ReplayError::UnknownCapability {
                    step_id: step_id.to_string(),
                    capability_id: capability_id.to_string(),
                }
            } else {
                ReplayError::NetworkError {
                    step_id: step_id.to_string(),
                    message: err,
                }
            }
        })?;

        let body_text = result.response_body.unwrap_or_default();
        let body_json = if body_text.is_empty() {
            None
        } else {
            serde_json::from_str(&body_text).ok()
        };
        let duration_ms = started.elapsed().as_millis() as u64;

        if !result.success {
            if result.status == 0 {
                return Err(ReplayError::NetworkError {
                    step_id: step_id.to_string(),
                    message: result.error.unwrap_or_else(|| {
                        "API replay failed before receiving an HTTP status".to_string()
                    }),
                });
            }

            let body_preview = if body_text.is_empty() {
                None
            } else {
                Some(truncate_body_preview(&body_text))
            };
            if is_http_success(result.status) {
                let detail = result
                    .verification
                    .map(|verification| verification.detail)
                    .or(result.error)
                    .unwrap_or_else(|| "Replay verification failed".to_string());
                return Err(ReplayError::VerificationFailed {
                    step_id: step_id.to_string(),
                    status: result.status,
                    detail,
                    body_preview,
                });
            }

            return Err(ReplayError::HttpFailure {
                step_id: step_id.to_string(),
                status: result.status,
                body_preview,
            });
        }

        Ok(StepExecutionResult {
            http_status: result.status,
            response_body_text: body_text,
            response_body_json: body_json,
            duration_ms,
            replay_request,
            request_params: params.clone(),
        })
    }

    /// Check the same replay eligibility gate that `ApiRunner` uses before
    /// executing HTTP. Workflow replay uses this to mark a workflow stale when a
    /// referenced capability was demoted or deleted after compilation.
    pub async fn can_replay(&self, origin_key: &str, capability_id: &str) -> Result<bool, String> {
        let runner = self.runner.lock().await;
        runner.can_replay(origin_key, capability_id)
    }
}

/// Cap a response body at 4 KB at a UTF-8 char boundary with a `…` marker.
/// Used by the engine when building `StepReplayOutcome::response_body_preview`.
pub fn truncate_body_preview(body: &str) -> String {
    if body.len() <= BODY_PREVIEW_CAP_BYTES {
        return body.to_string();
    }
    let mut cap = BODY_PREVIEW_CAP_BYTES;
    while cap > 0 && !body.is_char_boundary(cap) {
        cap -= 1;
    }
    let mut out = body[..cap].to_string();
    out.push('…');
    out
}

/// 2xx is success; everything else is a step failure (the engine returns
/// `ReplayError::HttpFailure`).
pub fn is_http_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Detect the specific error shape that `CapabilityStore::load` produces
/// when a capability id doesn't exist on disk. The store returns
/// `format!("Failed to read capability {id}: {io_err}")` and the io_err
/// for a missing file is `"No such file or directory (os error 2)"` on
/// Unix or `"The system cannot find the file specified. (os error 2)"`
/// on Windows. We match on the wrapping prefix + the "No such" /
/// "cannot find" substrings to cover both.
pub fn is_capability_not_found(err: &str) -> bool {
    err.starts_with("Failed to read capability ")
        && (err.contains("No such file or directory") || err.contains("cannot find the file"))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn step_effect_id_is_per_step_and_absent_without_an_attempt() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        // Same idiom as `engine.rs` — a scratch registry root, not the shared
        // temp dir, so a stray `registry_index.json` can't leak into the test.
        let temp = tempfile::tempdir().expect("temp dir");
        let runner = Arc::new(Mutex::new(
            crate::magician_v2::api_mining::replay::ApiRunner::with_base_path(temp.path())
                .expect("runner"),
        ));

        let attributed = StepExecutor::new(Arc::clone(&runner))
            .with_effect_id(Some("llm_call_abc:tool:toolu_01".to_string()));
        assert_eq!(
            attributed.step_effect_id("step-1").as_deref(),
            Some("llm_call_abc:tool:toolu_01:step:step-1")
        );
        assert_ne!(
            attributed.step_effect_id("step-1"),
            attributed.step_effect_id("step-2"),
            "each step of a replay is its own effect"
        );

        // A replay with no dispatch attempt behind it produces no key. That
        // means "not attributable" — never "safe to repeat".
        let unattributed = StepExecutor::new(runner);
        assert!(unattributed.step_effect_id("step-1").is_none());
    }

    #[test]
    fn truncate_body_preview_caps_at_4kb() {
        let big = "y".repeat(8 * 1024);
        let preview = truncate_body_preview(&big);
        assert!(preview.len() <= 4 * 1024 + 16);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn truncate_body_preview_passes_short_strings_through() {
        let small = r#"{"id":42}"#.to_string();
        assert_eq!(truncate_body_preview(&small), small);
    }

    #[test]
    fn classify_http_status_2xx_is_success() {
        assert!(is_http_success(200));
        assert!(is_http_success(204));
        assert!(is_http_success(299));
    }

    #[test]
    fn classify_http_status_non_2xx_is_failure() {
        assert!(!is_http_success(199));
        assert!(!is_http_success(300));
        assert!(!is_http_success(401));
        assert!(!is_http_success(500));
    }

    #[test]
    fn detects_capability_not_found_unix_form() {
        let err = "Failed to read capability cap_abc: No such file or directory (os error 2)";
        assert!(is_capability_not_found(err));
    }

    #[test]
    fn detects_capability_not_found_windows_form() {
        let err = "Failed to read capability cap_abc: The system cannot find the file specified. (os error 2)";
        assert!(is_capability_not_found(err));
    }

    #[test]
    fn ignores_other_capability_errors() {
        // Other I/O errors against the capability file should NOT map to
        // UnknownCapability — they're real I/O problems.
        assert!(!is_capability_not_found(
            "Failed to read capability cap_abc: Permission denied (os error 13)"
        ));
        // Errors from elsewhere in the pipeline (network, JSON parse, etc.)
        // should also not match.
        assert!(!is_capability_not_found("connection refused"));
        assert!(!is_capability_not_found(""));
    }
}
