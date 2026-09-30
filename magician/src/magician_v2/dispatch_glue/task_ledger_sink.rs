//! `TaskLedgerSink` implementation. Appends `LlmCallLedgerEvent`s into a
//! per-task on-disk JSONL ledger under the task's runtime directory.
//!
//! Phase 2 keeps the integration minimal — the events are persisted via a
//! best-effort file append. A future enhancement can wire this into the
//! existing `artifact_v2::append_runtime_ledger_event` path so projection
//! consumers see the events transactionally with other runtime state.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use magicllm::dispatch::{LlmCallLedgerEvent, TaskLedgerSink};
use magicllm::TaskRef;
use serde_json::json;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tracing::warn;

use crate::magician_v2::artifact_v2::service::ArtifactV2Service;

/// Appends ledger events to a per-task JSONL file under `<data_root>/llm_dispatch/<task_id>.jsonl`.
///
/// `_service` is reserved for a future enhancement that writes directly into
/// the task's runtime ledger via the artifact_v2 reducer (atomicity + cross-
/// projection consistency). For now we use plain JSONL on disk.
pub struct ArtifactV2TaskLedgerSink {
    _service: Arc<ArtifactV2Service>,
    base_dir: PathBuf,
}

impl ArtifactV2TaskLedgerSink {
    pub fn new(service: Arc<ArtifactV2Service>, base_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            _service: service,
            base_dir,
        })
    }

    fn jsonl_path(&self, task_id: &str) -> PathBuf {
        self.base_dir
            .join("llm_dispatch")
            .join(format!("{}.jsonl", sanitize(task_id)))
    }
}

#[async_trait]
impl TaskLedgerSink for ArtifactV2TaskLedgerSink {
    async fn append(&self, task_ref: &TaskRef, event: LlmCallLedgerEvent) {
        let path = self.jsonl_path(&task_ref.task_id);
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                warn!(?err, ?path, "failed to create ledger directory");
                return;
            }
        }
        let line = match serde_json::to_string(&json!({
            "task_ref": task_ref,
            "event": event,
            "ts_ms": chrono::Utc::now().timestamp_millis(),
        })) {
            Ok(s) => s + "\n",
            Err(err) => {
                warn!(?err, "failed to serialize ledger event");
                return;
            },
        };
        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            Ok(f) => f,
            Err(err) => {
                warn!(?err, ?path, "failed to open ledger file for append");
                return;
            },
        };
        if let Err(err) = file.write_all(line.as_bytes()).await {
            warn!(?err, ?path, "failed to append ledger line");
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => c,
            _ => '_',
        })
        .collect()
}
