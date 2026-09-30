//! TaskStateProvider — durable per-task state for cross-run persistence.
//!
//! A compiled capability provider that wraps `DurableArtifactStore::write()`
//! scoped to the currently executing task.  The agent calls this tool to
//! persist a JSON blob; on the next run the orchestrator pre-loads it and
//! injects it into the prompt automatically.
//!
//! The task identity is injected as `__task_id` in `resolved_params` by the
//! agentic executor at dispatch time — not stored on the provider — so
//! concurrent task executions sharing the same provider instance are safe.

use async_trait::async_trait;
use chrono::Utc;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifacts::durable_store::{
    open_local_durable_artifacts, DurableFrontmatter,
};
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::strategy::plan::PlanStep;

pub const TASK_STATE_TOOL_NAME: &str = "task_state";

/// Maximum size (in bytes) for serialized (pretty-printed) task state JSON.
///
/// 16 KB, not the original 4 KB: a typed checkpoint's fields are bounded
/// by `compact_durable_task_state_for_storage` (goal and criteria 768 B,
/// each micro-goal ~1 KB with its description, ids, status and timestamps,
/// the partial-yield record ~1.5 KB), so an ordinary four-goal state with
/// a terminal partial yield is 4–6 KB pretty-printed — over the old cap
/// with nothing left to compact. Run 17 (2026-09-21) finished its work and
/// then failed in the terminal Apply at 4,098 bytes: "refusing to discard
/// task graph or evidence" is the right refusal for an 80-goal runaway,
/// which 16 KB still refuses, and the wrong one for two bytes of
/// indentation on a normal state.
const MAX_STATE_BYTES: usize = 16_384;

/// Compact a JSON value to fit within `max_bytes` when pretty-printed.
///
/// Valid typed durable task state first bounds only verbose human-readable
/// fields while preserving task identity, graph structure, status and evidence.
/// Generic state retains the legacy fallback: find the largest top-level array
/// and binary-search for the maximum number of trailing elements (newest) that
/// fit. Only the largest array is trimmed.
///
/// Returns `(json_string, was_trimmed)`.  Errors if the structure has no
/// trimmable arrays and still exceeds the limit.
fn trim_to_fit(state: &serde_json::Value, max_bytes: usize) -> Result<(String, bool), String> {
    let (state, typed_state_was_compacted) =
        super::durable_task_state::compact_durable_task_state_for_storage(state, max_bytes)?;
    let json = serde_json::to_string_pretty(&state).map_err(|e| format!("serialize: {e}"))?;
    if json.len() <= max_bytes {
        return Ok((json, typed_state_was_compacted));
    }

    // Find the largest top-level array key.
    let largest_key = if let serde_json::Value::Object(map) = &state {
        map.iter()
            .filter_map(|(k, v)| v.as_array().map(|a| (k.clone(), a.len())))
            .max_by_key(|(_, len)| *len)
            .map(|(k, _)| k)
    } else {
        None
    };

    let largest_key = largest_key.ok_or_else(|| {
        format!(
            "state too large ({} bytes, max {}) and has no arrays to trim",
            json.len(),
            max_bytes
        )
    })?;

    // Binary search: find the maximum `keep` count (trailing elements) that fits.
    let full_len = state
        .as_object()
        .and_then(|m| m.get(&largest_key))
        .and_then(|v| v.as_array())
        .map_or(0, |a| a.len());

    if full_len <= 1 {
        return Err(format!(
            "state too large ({} bytes, max {}) and largest array has only {} element(s)",
            json.len(),
            max_bytes,
            full_len
        ));
    }

    let mut lo: usize = 1; // keep at least 1 element
    let mut hi: usize = full_len;
    let mut best_json: Option<String> = None;

    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        // Keep only the last `mid` elements (newest).
        let mut candidate = state.clone();
        if let Some(arr) = candidate
            .as_object_mut()
            .and_then(|m| m.get_mut(&largest_key))
            .and_then(|v| v.as_array_mut())
        {
            let start = arr.len().saturating_sub(mid);
            *arr = arr[start..].to_vec();
        }
        let candidate_json =
            serde_json::to_string_pretty(&candidate).map_err(|e| format!("serialize: {e}"))?;
        if candidate_json.len() <= max_bytes {
            best_json = Some(candidate_json);
            lo = mid + 1; // try keeping more
        } else {
            if mid == 1 {
                break; // even 1 element doesn't fit
            }
            hi = mid - 1;
        }
    }

    match best_json {
        Some(j) => Ok((j, true)),
        None => Err(format!(
            "state too large ({} bytes, max {}) even with array trimmed to 1 element",
            json.len(),
            max_bytes
        )),
    }
}

pub struct TaskStateProvider {
    workspace_layout: ArtifactV2Workspace,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for TaskStateProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskStateProvider")
            .field("workspace_layout", &self.workspace_layout)
            .field("pack_def", &self.pack_def)
            .finish()
    }
}

impl TaskStateProvider {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for TaskStateProvider {
    fn tool_name(&self) -> &str {
        TASK_STATE_TOOL_NAME
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: TASK_STATE_TOOL_NAME.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: TASK_STATE_TOOL_NAME.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params,
            _ => {
                return Err(ExecutionError::Step(
                    "task_state: unexpected action type".to_string(),
                ))
            },
        };

        // __task_id is injected by the agentic executor at dispatch time —
        // it comes from the per-execution `ActionExecutors::run_identity`, not
        // from a shared mutex, so concurrent executions are safe.
        let task_id = params
            .get("__task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ExecutionError::Step(
                    "task_state requires a task context (__task_id not present)".to_string(),
                )
            })?
            .to_string();
        let execution_id = params
            .get("__execution_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let principal = params
            .get("__principal")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ExecutionError::Step(
                    "task_state requires a scoped principal (__principal not present)".to_string(),
                )
            })?
            .to_string();
        let workspace = params
            .get("__workspace")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ExecutionError::Step(
                    "task_state requires a scoped workspace (__workspace not present)".to_string(),
                )
            })?
            .to_string();
        let agent_id = params
            .get("__agent_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let state = params.get("state").ok_or_else(|| {
            ExecutionError::Step("task_state: missing required 'state' parameter".to_string())
        })?;

        let (json, was_trimmed) = trim_to_fit(state, MAX_STATE_BYTES)
            .map_err(|e| ExecutionError::Step(format!("task_state: {e}")))?;

        let name = format!("{task_id}.json");
        let frontmatter = DurableFrontmatter {
            namespace: "task_state".to_string(),
            name: name.clone(),
            created_by: "task_state_provider".to_string(),
            last_updated_by: "task_state_provider".to_string(),
            last_updated: Utc::now(),
            content_type: Some("application/json".to_string()),
            source_task_id: Some(task_id),
            source_execution_id: execution_id,
            source_workflow_instance_id: None,
            source_run_id: None,
            source_cycle_id: None,
            source_agent_id: agent_id,
            producer_stage: None,
        };

        let durable_store =
            open_local_durable_artifacts(&self.workspace_layout, &principal, &workspace)
                .map_err(|e| ExecutionError::Step(format!("task_state: scope init failed: {e}")))?;

        durable_store
            .write("task_state", &name, &json, frontmatter)
            .await
            .map_err(|e| ExecutionError::Step(format!("task_state: write failed: {e}")))?;

        let message = if was_trimmed {
            format!(
                "state persisted (checkpoint was compacted or trimmed to fit {} byte limit)",
                MAX_STATE_BYTES
            )
        } else {
            "state persisted".to_string()
        };

        Ok(ActionResult::Text { content: message })
    }

    fn default_timeout_secs(&self) -> u64 {
        self.pack_def
            .as_ref()
            .and_then(|d| d.execution.as_ref())
            .and_then(|e| e.default_timeout_secs)
            .unwrap_or(5)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_provider(tmp: &TempDir) -> TaskStateProvider {
        TaskStateProvider::new(ArtifactV2Workspace::new(tmp.path()))
    }

    fn make_action(task_id: Option<&str>, state: serde_json::Value) -> ExecutableAction {
        let mut params = HashMap::new();
        params.insert("state".to_string(), state);
        if let Some(tid) = task_id {
            params.insert("__task_id".to_string(), json!(tid));
            params.insert("__principal".to_string(), json!("test-principal"));
            params.insert("__workspace".to_string(), json!("test-workspace"));
        }
        ExecutableAction::Pack {
            capability_name: "task_state".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "task_state".to_string(),
            },
            resolved_params: params,
        }
    }

    #[tokio::test]
    async fn test_write_valid_state() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        let action = make_action(Some("test-task-1"), json!({"last_seen_id": 42}));
        let result = provider.execute(&action, None, 5).await;
        assert!(result.is_ok(), "write should succeed: {:?}", result);

        // Verify file was written with correct content
        let store = open_local_durable_artifacts(
            &ArtifactV2Workspace::new(tmp.path()),
            "test-principal",
            "test-workspace",
        )
        .unwrap();
        let (frontmatter, body) = store.read("task_state", "test-task-1.json").await.unwrap();
        assert_eq!(frontmatter.source_task_id, Some("test-task-1".to_string()));
        assert_eq!(
            frontmatter.content_type,
            Some("application/json".to_string())
        );

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, json!({"last_seen_id": 42}));
    }

    #[tokio::test]
    async fn test_write_persists_execution_id() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        let mut params = HashMap::new();
        params.insert("state".to_string(), json!({"status": "ok"}));
        params.insert("__task_id".to_string(), json!("test-task-legacy"));
        params.insert("__execution_id".to_string(), json!("exec-legacy-1"));
        params.insert("__principal".to_string(), json!("test-principal"));
        params.insert("__workspace".to_string(), json!("test-workspace"));
        let action = ExecutableAction::Pack {
            capability_name: "task_state".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "task_state".to_string(),
            },
            resolved_params: params,
        };

        provider.execute(&action, None, 5).await.unwrap();

        let store = open_local_durable_artifacts(
            &ArtifactV2Workspace::new(tmp.path()),
            "test-principal",
            "test-workspace",
        )
        .unwrap();
        let (frontmatter, _) = store
            .read("task_state", "test-task-legacy.json")
            .await
            .unwrap();
        assert_eq!(
            frontmatter.source_execution_id.as_deref(),
            Some("exec-legacy-1")
        );
    }

    #[tokio::test]
    async fn test_oversized_array_trimmed_not_rejected() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        // Create a state with a large array that exceeds the cap when
        // pretty-printed (one element per line, ~10 bytes each).
        let big_array: Vec<u64> = (0..2_500).collect();
        let action = make_action(
            Some("test-task-2"),
            json!({"ids": big_array, "cursor": 999}),
        );
        let result = provider.execute(&action, None, 5).await;

        // Should succeed (trimmed), not error
        assert!(
            result.is_ok(),
            "oversized array should be trimmed: {:?}",
            result
        );
        let msg = match result.unwrap() {
            ActionResult::Text { content } => content,
            _ => panic!("expected Text result"),
        };
        assert!(
            msg.contains("trimmed"),
            "message should mention trimming: {msg}"
        );

        // Read back — should be valid JSON with fewer elements, non-array fields intact
        let store = open_local_durable_artifacts(
            &ArtifactV2Workspace::new(tmp.path()),
            "test-principal",
            "test-workspace",
        )
        .unwrap();
        let (_fm, body) = store.read("task_state", "test-task-2.json").await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["cursor"], json!(999)); // non-array field preserved
        let trimmed_ids = parsed["ids"].as_array().unwrap();
        assert!(trimmed_ids.len() < 2_500, "array should be shorter");
        assert!(!trimmed_ids.is_empty(), "should keep at least 1 element");
        // Trimming keeps the tail (newest entries)
        assert_eq!(*trimmed_ids.last().unwrap(), json!(2_499));
    }

    #[tokio::test]
    async fn test_oversized_typed_checkpoint_is_compacted_and_persisted() {
        use crate::magician_v2::execution::durable_task_state::{
            synthesize_durable_task_state, validate_durable_task_state,
        };

        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);
        let goal = format!(
            "USER INTENT: inspect the repository. {} GOAL TAIL",
            "g".repeat(9_000)
        );
        let success = format!(
            "SUCCESS: return a supported explanation. {} SUCCESS TAIL",
            "s".repeat(9_000)
        );
        let mut state =
            synthesize_durable_task_state("test-task-terminal", &goal, &success, Some("cto"));
        state["metadata"]["latest_partial_yield"] = json!({
            "summary": format!("{} SUMMARY TAIL", "p".repeat(900)),
            "open": [format!("{} OPEN TAIL", "o".repeat(500))],
            "blockers": [format!("{} BLOCKER TAIL", "b".repeat(500))],
            "recorded_at": "2026-08-15T22:27:00Z"
        });
        assert!(serde_json::to_string_pretty(&state).unwrap().len() > MAX_STATE_BYTES);

        let result = provider
            .execute(&make_action(Some("test-task-terminal"), state), None, 5)
            .await
            .expect("typed terminal checkpoint should persist");
        let ActionResult::Text { content } = result else {
            panic!("expected text result");
        };
        assert!(content.contains("compacted"));

        let store = open_local_durable_artifacts(
            &ArtifactV2Workspace::new(tmp.path()),
            "test-principal",
            "test-workspace",
        )
        .unwrap();
        let (_frontmatter, body) = store
            .read("task_state", "test-task-terminal.json")
            .await
            .unwrap();
        assert!(
            body.len() <= MAX_STATE_BYTES,
            "persisted bytes={}",
            body.len()
        );
        let persisted: serde_json::Value = serde_json::from_str(&body).unwrap();
        validate_durable_task_state(&persisted, Some("test-task-terminal")).unwrap();
        assert_eq!(persisted["active_micro_goal_id"], json!("mg_initial"));
        assert_eq!(persisted["micro_goals"].as_array().unwrap().len(), 1);
        assert_eq!(persisted["micro_goals"][0]["id"], json!("mg_initial"));
        assert_eq!(persisted["metadata"]["storage_compacted"], json!(true));
    }

    #[tokio::test]
    async fn test_oversized_structure_without_arrays_errors() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        // Create a state with a single huge string (no arrays to trim),
        // well past the 16 KB cap.
        let big_value: String = "x".repeat(20_000);
        let action = make_action(Some("test-task-2b"), json!({"data": big_value}));
        let result = provider.execute(&action, None, 5).await;

        assert!(
            result.is_err(),
            "non-trimmable oversized state should error"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too large"),
            "error should mention size: {err}"
        );
    }

    #[tokio::test]
    async fn test_write_without_task_id_errors() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        let action = make_action(None, json!({"test": 1}));
        let result = provider.execute(&action, None, 5).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("__task_id not present"),
            "error should mention missing __task_id: {err}"
        );
    }

    #[tokio::test]
    async fn test_write_overwrites_prior_state() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        // Write initial state
        let action1 = make_action(Some("test-task-3"), json!({"version": 1}));
        provider.execute(&action1, None, 5).await.unwrap();

        // Overwrite with new state
        let action2 = make_action(Some("test-task-3"), json!({"version": 2}));
        provider.execute(&action2, None, 5).await.unwrap();

        // Read back — should be version 2
        let store = open_local_durable_artifacts(
            &ArtifactV2Workspace::new(tmp.path()),
            "test-principal",
            "test-workspace",
        )
        .unwrap();
        let (_fm, body) = store.read("task_state", "test-task-3.json").await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, json!({"version": 2}));
    }

    #[test]
    fn test_lower_produces_pack_action() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        let mut params = HashMap::new();
        params.insert("state".to_string(), json!({"key": "value"}));

        let step = PlanStep {
            tool: Some("task_state".to_string()),
            parameters: params,
            ..Default::default()
        };

        let gated_action = provider.lower(&step).unwrap();
        match gated_action.inner_action() {
            ExecutableAction::Pack {
                capability_name, ..
            } => {
                assert_eq!(capability_name, "task_state");
            },
            _ => panic!("expected Pack action"),
        }
    }

    #[tokio::test]
    async fn test_missing_state_parameter_errors() {
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        // Action with __task_id but no "state" parameter
        let mut params = HashMap::new();
        params.insert("__task_id".to_string(), json!("test-task-no-state"));
        params.insert("__principal".to_string(), json!("test-principal"));
        params.insert("__workspace".to_string(), json!("test-workspace"));
        let action = ExecutableAction::Pack {
            capability_name: "task_state".to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: "task_state".to_string(),
            },
            resolved_params: params,
        };
        let result = provider.execute(&action, None, 5).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("missing required 'state'"),
            "error should mention missing state: {err}"
        );
    }

    #[test]
    fn test_trim_to_fit_under_limit_no_trim() {
        let state = json!({"key": "value"});
        let (json, was_trimmed) = trim_to_fit(&state, 4096).unwrap();
        assert!(!was_trimmed);
        assert!(json.len() <= 4096);
    }

    #[test]
    fn test_trim_to_fit_exactly_at_limit() {
        // Build a state that's exactly at or just under the limit
        let state = json!({"key": "value"});
        let json_str = serde_json::to_string_pretty(&state).unwrap();
        let (result, was_trimmed) = trim_to_fit(&state, json_str.len()).unwrap();
        assert!(!was_trimmed);
        assert_eq!(result.len(), json_str.len());
    }

    #[test]
    fn test_trim_to_fit_small_array_two_elements() {
        // Array with 2 elements; one must be dropped to fit
        // Use a very small limit to force trimming
        let state = json!({"arr": [100, 200], "x": 1});
        let full = serde_json::to_string_pretty(&state).unwrap();
        // Set limit smaller than full but enough for 1 element
        let limit = full.len() - 5;
        let (result, was_trimmed) = trim_to_fit(&state, limit).unwrap();
        assert!(was_trimmed);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let arr = parsed["arr"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        // Should keep the tail (newest) = 200
        assert_eq!(arr[0], json!(200));
    }

    #[test]
    fn test_trim_to_fit_non_object_rejected() {
        // A JSON array at the top level (not an object) — can't trim
        let state = json!([1, 2, 3, 4, 5]);
        let result = trim_to_fit(&state, 5); // very small limit
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no arrays to trim"));
    }

    #[tokio::test]
    async fn test_concurrent_tasks_isolated() {
        // Two providers sharing the same store but writing under different task_ids.
        // Verifies no cross-contamination.
        let tmp = TempDir::new().unwrap();
        let provider = make_provider(&tmp);

        let action_a = make_action(Some("task-a"), json!({"owner": "a"}));
        let action_b = make_action(Some("task-b"), json!({"owner": "b"}));

        provider.execute(&action_a, None, 5).await.unwrap();
        provider.execute(&action_b, None, 5).await.unwrap();

        let store = open_local_durable_artifacts(
            &ArtifactV2Workspace::new(tmp.path()),
            "test-principal",
            "test-workspace",
        )
        .unwrap();

        let (_fm, body_a) = store.read("task_state", "task-a.json").await.unwrap();
        let parsed_a: serde_json::Value = serde_json::from_str(&body_a).unwrap();
        assert_eq!(parsed_a, json!({"owner": "a"}));

        let (_fm, body_b) = store.read("task_state", "task-b.json").await.unwrap();
        let parsed_b: serde_json::Value = serde_json::from_str(&body_b).unwrap();
        assert_eq!(parsed_b, json!({"owner": "b"}));
    }
}
