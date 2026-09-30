//! Exact-execution crash receipts for the task-start API rail.
//!
//! Callers hold the canonical execution lifecycle exclusion across load/save
//! and HTTP. A write-intent receipt precedes dispatch; a terminal receipt
//! precedes Runtime settlement. Neither recipe deletion nor disabling mining
//! removes this recovery authority. Released intents prove no write was sent.

use serde::{Deserialize, Serialize};

use super::{
    models::ExecutionOutcomeSnapshot, service::ArtifactV2Error, workspace::ArtifactV2Workspace,
};
use crate::magician_v2::{
    analytics::llm_trace_content::scoped_content_fingerprint, storage::ExecutionRun,
};

const DOMAIN: &str = "magician.task-recipe-replay.v1";
const MAX_RECEIPT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RecipeReplayReceiptState {
    WriteIntent { recipe_id: String },
    Terminal { outcome: ExecutionOutcomeSnapshot },
    Released,
}

impl RecipeReplayReceiptState {
    pub(crate) fn recovery_outcome(&self) -> Option<ExecutionOutcomeSnapshot> {
        match self {
            Self::Terminal { outcome } => Some(outcome.clone()),
            Self::WriteIntent { recipe_id } => Some(ExecutionOutcomeSnapshot {
                execution_status: "failed".into(),
                task_status: "failed".into(),
                outcome_type: "recipe_replay_interrupted".into(),
                outcome_summary: format!(
                    "API recipe {recipe_id} was interrupted after write admission. Its server outcome may be uncertain. Magician did not repeat the mutation or retry it in the browser; verify the remote state before starting another run."
                ),
                iterations_used: Some(0),
                is_terminal: true,
                completion_kind: None,
                open_items: Vec::new(),
            }),
            Self::Released => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    principal: String,
    workspace: String,
    task_id: Option<String>,
    execution_id: String,
    created_at: i64,
    root_execution_id: Option<String>,
    parent_execution_id: Option<String>,
    agent_id: String,
}

impl From<&ExecutionRun> for Binding {
    fn from(runtime: &ExecutionRun) -> Self {
        Self {
            principal: runtime.principal.clone(),
            workspace: runtime.workspace.clone(),
            task_id: runtime.task_id.clone(),
            execution_id: runtime.id.clone(),
            created_at: runtime.created_at,
            root_execution_id: runtime.root_execution_id.clone(),
            parent_execution_id: runtime.parent_execution_id.clone(),
            agent_id: runtime.active_owner_agent_id.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    binding: Binding,
    state: RecipeReplayReceiptState,
    hmac_sha256: String,
}

fn path(layout: &ArtifactV2Workspace, runtime: &ExecutionRun) -> std::path::PathBuf {
    let key = blake3::hash(
        format!(
            "{}\0{}",
            runtime.task_id.as_deref().unwrap_or(""),
            runtime.id
        )
        .as_bytes(),
    );
    layout
        .scope_root(&runtime.principal, &runtime.workspace)
        .join("restricted")
        .join("task_recipe_replays")
        .join(format!("{}.json", key.to_hex()))
}

fn seal(
    layout: &ArtifactV2Workspace,
    binding: &Binding,
    state: &RecipeReplayReceiptState,
) -> Result<String, ArtifactV2Error> {
    let bytes = serde_json::to_vec(&(DOMAIN, binding, state))?;
    if bytes.len() > MAX_RECEIPT_BYTES - 1024 {
        return Err(ArtifactV2Error::InvalidRequest(
            "recipe_replay_receipt_too_large".into(),
        ));
    }
    scoped_content_fingerprint(
        layout,
        &magicllm::LlmScope::new(&binding.principal, &binding.workspace),
        &bytes,
    )
    .map_err(|error| ArtifactV2Error::Runtime(error.to_string()))
}

pub(crate) async fn load(
    layout: &ArtifactV2Workspace,
    runtime: &ExecutionRun,
) -> Result<Option<RecipeReplayReceiptState>, ArtifactV2Error> {
    let path = path(layout, runtime);
    match layout.symlink_metadata_path(&path).await? {
        None => return Ok(None),
        Some(metadata) if metadata.is_file() && metadata.len() <= MAX_RECEIPT_BYTES as u64 => {},
        Some(_) => {
            return Err(ArtifactV2Error::InvalidRequest(
                "recipe_replay_receipt_invalid_file".into(),
            ))
        },
    }
    let bytes = layout
        .read_bounded_path(&path, MAX_RECEIPT_BYTES as u64)
        .await?;
    let receipt: Receipt = serde_json::from_slice(&bytes)?;
    let mut expected_binding = Binding::from(runtime);
    if matches!(&receipt.state, RecipeReplayReceiptState::Released) {
        // A safe browser handoff can legitimately delegate to another agent.
        // Released receipts carry no terminal/mutation authority for that owner.
        expected_binding.agent_id = receipt.binding.agent_id.clone();
    }
    if receipt.binding != expected_binding {
        return Err(ArtifactV2Error::InvalidRequest(
            "recipe_replay_receipt_binding_mismatch".into(),
        ));
    }
    let expected = seal(layout, &receipt.binding, &receipt.state)?;
    if expected.len() != receipt.hmac_sha256.len()
        || expected
            .bytes()
            .zip(receipt.hmac_sha256.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            != 0
    {
        return Err(ArtifactV2Error::InvalidRequest(
            "recipe_replay_receipt_seal_mismatch".into(),
        ));
    }
    Ok(Some(receipt.state))
}

pub(crate) async fn save(
    layout: &ArtifactV2Workspace,
    runtime: &ExecutionRun,
    state: RecipeReplayReceiptState,
) -> Result<(), ArtifactV2Error> {
    let binding = Binding::from(runtime);
    let hmac_sha256 = seal(layout, &binding, &state)?;
    let receipt = Receipt {
        binding,
        state,
        hmac_sha256,
    };
    let bytes = serde_json::to_vec(&receipt)?;
    let path = path(layout, runtime);
    layout
        .create_dir_all_path(path.parent().expect("receipt path has a parent"))
        .await?;
    layout.write_atomic_path(&path, &bytes).await?;
    // Provider publication errors must not permit dispatch, including a write
    // acknowledged by a provider but not visible to a fresh owner.
    let visible = load(layout, runtime)
        .await?
        .ok_or_else(|| ArtifactV2Error::Runtime("recipe_replay_receipt_not_visible".into()))?;
    if serde_json::to_value(visible)? != serde_json::to_value(&receipt.state)? {
        return Err(ArtifactV2Error::Runtime(
            "recipe_replay_receipt_changed".into(),
        ));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn runtime() -> ExecutionRun {
        serde_json::from_value(serde_json::json!({
            "id": "exec", "principal": "owner", "workspace": "default",
            "task_id": "task", "root_execution_id": "exec",
            "active_owner_agent_id": "agent", "waiting_state": "Runnable",
            "created_at": 1, "updated_at": 2
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn interrupted_write_survives_reopening_and_never_becomes_a_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::with_local_file_provider(tmp.path());
        let runtime = runtime();
        save(
            &layout,
            &runtime,
            RecipeReplayReceiptState::WriteIntent {
                recipe_id: "recipe".into(),
            },
        )
        .await
        .unwrap();
        let reopened = ArtifactV2Workspace::with_local_file_provider(tmp.path());
        let outcome = load(&reopened, &runtime)
            .await
            .unwrap()
            .unwrap()
            .recovery_outcome()
            .unwrap();
        assert_eq!(outcome.execution_status, "failed");
        assert_eq!(outcome.outcome_type, "recipe_replay_interrupted");
        assert!(outcome.is_terminal);
        assert!(outcome.outcome_summary.contains("did not repeat"));
    }

    #[tokio::test]
    async fn terminal_answer_survives_runtime_status_and_timestamp_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::with_local_file_provider(tmp.path());
        let mut runtime = runtime();
        let outcome = super::super::recipe_replay_hook::RecipeReplayAttempt::Completed {
            answer_summary: "price: 43\navailable: false".into(),
            recipe_id: "recipe".into(),
            steps: 2,
        }
        .terminal_outcome(false)
        .unwrap();
        save(
            &layout,
            &runtime,
            RecipeReplayReceiptState::Terminal {
                outcome: outcome.clone(),
            },
        )
        .await
        .unwrap();
        runtime.waiting_state = crate::magician_v2::storage::WaitingState::Completed;
        runtime.updated_at += 10;
        let restored = load(&layout, &runtime)
            .await
            .unwrap()
            .unwrap()
            .recovery_outcome()
            .unwrap();
        assert_eq!(
            serde_json::to_value(restored).unwrap(),
            serde_json::to_value(outcome).unwrap()
        );
    }

    #[tokio::test]
    async fn receipt_rejects_new_generation_cross_scope_and_tampered_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::with_local_file_provider(tmp.path());
        let runtime = runtime();
        save(
            &layout,
            &runtime,
            RecipeReplayReceiptState::WriteIntent {
                recipe_id: "recipe".into(),
            },
        )
        .await
        .unwrap();
        let mut changed = runtime.clone();
        changed.created_at += 1;
        assert!(load(&layout, &changed).await.is_err());

        let original = layout.read_path(path(&layout, &runtime)).await.unwrap();
        changed = runtime.clone();
        changed.workspace = "other".into();
        let copied_path = path(&layout, &changed);
        layout
            .create_dir_all_path(copied_path.parent().unwrap())
            .await
            .unwrap();
        layout
            .write_atomic_path(copied_path, &original)
            .await
            .unwrap();
        assert!(load(&layout, &changed).await.is_err());

        let mut receipt: Receipt = serde_json::from_slice(&original).unwrap();
        receipt.state = RecipeReplayReceiptState::Released;
        layout
            .write_json_atomic_path(path(&layout, &runtime), &receipt)
            .await
            .unwrap();
        assert!(load(&layout, &runtime).await.is_err());
    }

    #[tokio::test]
    async fn only_durable_release_allows_safe_pre_send_browser_handoff() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::with_local_file_provider(tmp.path());
        let mut runtime = runtime();
        save(
            &layout,
            &runtime,
            RecipeReplayReceiptState::WriteIntent {
                recipe_id: "recipe".into(),
            },
        )
        .await
        .unwrap();
        assert!(load(&layout, &runtime)
            .await
            .unwrap()
            .unwrap()
            .recovery_outcome()
            .is_some());
        save(&layout, &runtime, RecipeReplayReceiptState::Released)
            .await
            .unwrap();
        runtime.active_owner_agent_id = "browser-delegate".into();
        assert!(load(&layout, &runtime)
            .await
            .unwrap()
            .unwrap()
            .recovery_outcome()
            .is_none());
    }

    #[tokio::test]
    async fn malformed_receipt_is_not_a_cache_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::with_local_file_provider(tmp.path());
        let runtime = runtime();
        assert!(load(&layout, &runtime).await.unwrap().is_none());
        let path = path(&layout, &runtime);
        layout
            .create_dir_all_path(path.parent().unwrap())
            .await
            .unwrap();
        layout.write_atomic_path(path, b"{truncated").await.unwrap();
        assert!(load(&layout, &runtime).await.is_err());
    }
}
