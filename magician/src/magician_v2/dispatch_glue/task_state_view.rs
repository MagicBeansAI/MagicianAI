//! `TaskStateView` implementation backed by `ArtifactV2Service`.

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use magicllm::dispatch::{CancellationToken, TaskSnapshot, TaskStateView};
use magicllm::TaskRef;

use crate::magician_v2::artifact_v2::service::ArtifactV2Service;

/// Adapter that resolves task lifecycle status from the artifact_v2 service.
///
/// The cancellation-token side maintains a per-task `CancellationToken` so
/// callers (the worker's in-flight `select!`) can race the provider call
/// against task cancel. Tokens are created lazily on first subscription.
pub struct ArtifactV2TaskStateView {
    service: Arc<ArtifactV2Service>,
    tokens: CancelTokenTree,
}

impl ArtifactV2TaskStateView {
    pub fn new(service: Arc<ArtifactV2Service>) -> Arc<Self> {
        Arc::new(Self {
            service,
            tokens: CancelTokenTree::default(),
        })
    }

    /// Fire the cancellation token for a task. Called by the orchestrator
    /// when a task transitions to cancelled / failed.
    pub fn fire_cancel(&self, task_id: &str) {
        self.tokens.fire(task_id);
    }
}

/// One token per execution, derived from its root's, derived from its
/// task's: cancelling a task fires every job of every execution under it,
/// cancelling a root fires every descendant's, cancelling an execution
/// fires its own, and cancelling a child never touches its parent.
///
/// The first cut registered ONE shared token under every id a job carried
/// — task, execution and root alike — so a delegated child's jobs pinned
/// the child's token under the parent's root id, and force-failing that
/// child (`cancel_execution`, the V2 rule that a delegated child may not
/// pause for user input) cancelled the token the parent's own jobs then
/// found under their id. Every later parent decision died at pickup as
/// `task_cancelled_in_flight` — three parse errors, run failed — for the
/// rest of the process, since the map never forgets. The second cut keyed
/// an execution's own token under its `task_id` too, and an agentic job's
/// `task_id` is the persisted task the parent and child share, so the
/// sharing came back through that id. `child_token()` is the right
/// direction at every level: it fires when its parent fires and never the
/// other way.
#[derive(Default)]
struct CancelTokenTree {
    tokens: DashMap<String, CancellationToken>,
}

impl CancelTokenTree {
    fn fire(&self, id: &str) {
        if let Some(entry) = self.tokens.get(id) {
            entry.value().cancel();
        }
    }

    /// The token registered under `id`, or a fresh child of `parent` (a
    /// fresh root when there is no parent) registered there.
    fn level(&self, id: &str, parent: Option<&CancellationToken>) -> CancellationToken {
        self.tokens
            .entry(id.to_string())
            .or_insert_with(|| {
                parent
                    .map(CancellationToken::child_token)
                    .unwrap_or_else(CancellationToken::new)
            })
            .clone()
    }

    fn subscribe(&self, task_ref: &TaskRef) -> CancellationToken {
        let present = |id: &str| (!id.is_empty()).then(|| id.to_string());
        let task = present(&task_ref.task_id);
        let root = task_ref
            .root_execution_id
            .as_deref()
            .and_then(present)
            .filter(|id| Some(id) != task.as_ref());
        let execution = task_ref
            .execution_id
            .as_deref()
            .and_then(present)
            .filter(|id| Some(id) != task.as_ref() && Some(id) != root.as_ref());
        let mut token: Option<CancellationToken> = None;
        for id in [task, root, execution].into_iter().flatten() {
            token = Some(self.level(&id, token.as_ref()));
        }
        token.unwrap_or_else(CancellationToken::new)
    }
}

#[async_trait]
impl TaskStateView for ArtifactV2TaskStateView {
    async fn snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, String> {
        match self.service.get_task_by_id(task_id).await {
            Ok(Some((_scope, task))) => {
                let status = task.state.status.as_str();
                Ok(Some(TaskSnapshot {
                    is_cancelled_or_terminal_failed: matches!(status, "cancelled" | "failed"),
                    cancel_reason: None,
                }))
            },
            // The id did not resolve to an artifact_v2 *task*. In practice the
            // dispatch-queue `task_ref` carries a runtime *execution_id*
            // (`OperationLlmRouter::with_task_context` ← `cancel_execution`),
            // which lives in a disjoint id space from artifact task ids and never
            // has a task manifest — so this arm is hit for EVERY execution-scoped
            // LLM job. Returning `Ok(None)` here would make the worker's
            // pre-dispatch gate tombstone the job as `TaskMissing`, failing every
            // such call. We must PROCEED instead: cancellation for these jobs is
            // driven entirely by the in-memory paths that key on the SAME id —
            // `queue.cancel_task(id)` records a cancellation intent the worker
            // consumes at pickup, and `fire_cancel(id)` aborts the in-flight
            // provider call — so the task-store pre-dispatch gate is redundant for
            // them and must not reject. (A genuinely-deleted real task would also
            // proceed and have its result discarded — benign and not currently
            // reachable, since no producer tags jobs with a real task_id.)
            Ok(None) => Ok(Some(TaskSnapshot {
                is_cancelled_or_terminal_failed: false,
                cancel_reason: None,
            })),
            Err(err) => Err(err.to_string()),
        }
    }

    /// One token per execution, derived from its root's: cancelling an
    /// execution fires its own jobs, cancelling a root fires every
    /// descendant's, and cancelling a child leaves its parent alone.
    ///
    /// The first cut registered ONE shared token under every id a job
    /// carried — task, execution and root alike — so a delegated child's
    /// jobs pinned the child's token under the parent's root id, and
    /// force-failing that child (`cancel_execution`, the V2 rule that a
    /// delegated child may not pause for user input) cancelled the token
    /// the parent's own jobs then found under their id. Every later parent
    /// decision died at pickup as `task_cancelled_in_flight` — three parse
    /// errors, run failed — for the rest of the process, since the map
    /// never forgets. `child_token()` is the right direction: it fires when
    /// its parent fires and never the other way.
    fn subscribe_cancel(&self, task_ref: Option<&TaskRef>) -> CancellationToken {
        match task_ref {
            Some(task_ref) => self.tokens.subscribe(task_ref),
            None => CancellationToken::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agentic job's shape: the persisted task shared by every
    /// execution under it, the root execution, and the job's own.
    fn job(execution_id: &str, root: &str) -> TaskRef {
        TaskRef::task("task_shared").with_execution(root, execution_id)
    }

    #[test]
    fn cancelling_a_child_execution_leaves_its_parent_running() {
        let tree = CancelTokenTree::default();
        let child = tree.subscribe(&job("exec-deleg-child", "exec_parent"));
        tree.fire("exec-deleg-child");
        assert!(child.is_cancelled());
        let parent = tree.subscribe(&job("exec_parent", "exec_parent"));
        assert!(
            !parent.is_cancelled(),
            "a child's cancel poisoned its parent"
        );
    }

    #[test]
    fn cancelling_a_root_fires_every_descendant() {
        let tree = CancelTokenTree::default();
        let child = tree.subscribe(&job("exec-deleg-child", "exec_parent"));
        let parent = tree.subscribe(&job("exec_parent", "exec_parent"));
        tree.fire("exec_parent");
        assert!(parent.is_cancelled());
        assert!(child.is_cancelled());
    }

    #[test]
    fn cancelling_the_task_fires_the_parent_and_every_child() {
        let tree = CancelTokenTree::default();
        let child = tree.subscribe(&job("exec-deleg-child", "exec_parent"));
        let parent = tree.subscribe(&job("exec_parent", "exec_parent"));
        tree.fire("task_shared");
        assert!(parent.is_cancelled() && child.is_cancelled());
    }

    #[test]
    fn jobs_of_one_execution_share_a_token() {
        let tree = CancelTokenTree::default();
        let first = tree.subscribe(&job("exec-deleg-child", "exec_parent"));
        let second = tree.subscribe(&job("exec-deleg-child", "exec_parent"));
        tree.fire("exec-deleg-child");
        assert!(first.is_cancelled() && second.is_cancelled());
    }
}
