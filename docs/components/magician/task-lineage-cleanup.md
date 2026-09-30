# Task Lineage Cleanup

How deletion of a task propagates through the artifact-V2 storage layout. Source of truth: `magician/src/magician_v2/artifact_v2/service.rs`.

## Two storage roots

Tasks live in one of two parallel folders, distinguished by visibility:

- `<scope>/tasks/<task_id>/` — user-visible. Shown on `/tasks`.
- `<scope>/internal_tasks/<task_id>/` — internal / non-user-visible. Shown only on `/tasks?type=internal`.

Both folders hold the same internal shape: `manifest.json`, `state/`, `refs/`, `outputs/`, `executions/<exec_id>/`, plan storage, event journals. There is no archival state — a task either exists or it doesn't.

## Delete entry points

- `archive_task_with_options(scope, task_id, remove_files=true)` — the canonical "delete with files" call. Used by:
  - `task_api_v3.rs` `delete_task` handler (`DELETE /api/magician/v3/tasks/{id}`)
  - `chat/service.rs` `delete_task` LLM tool
  - `progress_channels/channels/chat.rs` chat-pack teardown
  - `web_api.rs` cycle-conflict rollback
- `delete_internal_task(scope, task_id)` — internal Tasks row delete; thin wrapper that delegates to the same dual-folder cleanup.

Both routes funnel into `remove_task_dir_for_delete`, which:

1. Acquires the task lifecycle lock under
   `<scope>/.task_lifecycle/locks/<task_id>.lock`. The lock lives outside the
   deletable tree, so it cannot disappear beneath a waiting writer.
2. Validates both possible task directories, including canonical parent-root
   containment, before exposing any deletion marker.
3. Serializes with canonical event appends across processes through
   `<scope>/.task_lifecycle/event_locks/<task_id>.lock`, then writes the durable
   fence `<scope>/.task_lifecycle/deleted/<task_id>.deleted`.
4. Removes whichever task directory (or both) exists. A transient `ENOTEMPTY`
   receives three bounded retries. If physical deletion fails and any task
   directory survives, the event writer removes the marker under the same
   cross-process lock so the surviving task is not permanently bricked. A
   partial dual-root failure does not deindex or clean external references for
   the surviving authoritative copy.
5. Only after neither root retains the task, runs
   `cleanup_task_external_artifacts` and deindexes the task.

All task record/reducer guards and task-bound execution-document writers check
the deletion fence after acquiring the same external lifecycle lock. Canonical
event appends check it under both their in-process append lock and the scoped
cross-process event lock. Event sequence cursors also validate the JSONL file
length under that lock, so independent processes cannot reuse a stale cached
sequence after another writer appends. A summary, queued runtime event,
or resumed execution that wakes after deletion therefore fails closed as
`TaskNotFound`/`ExecutionNotFound`; it cannot recreate `tasks/<id>` or race the
directory removal.

## What cascade cleanup removes

`cleanup_task_external_artifacts(scope, task_id)` is best-effort: failures are
logged (`warn!`) but never fail the delete. A later idempotent delete still runs
external cleanup after confirming that neither task root exists, so the retry
can remove progress, publication, and feed artifacts left by a prior partial
cleanup even though the task directory is already gone.

Cleaned up:

- **Task progress-channel event log** under
  `progress_channels/events/task:<task_id>.jsonl`.

- **Published surfaces** that referenced this task (`record.task_id == Some(task_id)`):
  - Per-surface JSON file under `published_surfaces/<surface_id>.json`
  - Materialized muij layout via `muij_storage.delete_surface_layout(document_key)`
  - The `published_surfaces.json` index is rewritten to drop the orphan entries
- **Feed projection summary** for the task (`adapter.remove_task_summary`), done here as defense-in-depth so cleanup doesn't depend on caller discipline.

## What cascade cleanup intentionally does NOT remove

- **Episode records** (`memory/agent_episodes/<agent_id>/...`). Episodes are agent-scoped, not task-scoped — they capture what an agent learned from running a task. The agent's memory survives the task's deletion by design; otherwise re-running a similar task would lose the agent's accumulated experience.
- **Scoped chat-pack execution dirs** (`<scope>/executions/chat-pack-exec-…/`). These are cleaned up by the chat-session deletion path (`chat/storage.rs::delete_session`), not by task deletion. The marker file inside each exec dir records which chat session owns it.
- **LLM call / observability logs**. Stored in a separate event stream keyed by execution_id, not task_id. They're trimmed by their own retention policy, not by task deletion.

## Failure semantics

| Failure | Consequence |
| --- | --- |
| `task_id` validation fails (path traversal, slashes) | `InvalidRequest` — nothing touched. |
| Task dir is missing entirely | `Ok(())`. Idempotent delete. |
| Task dir is on disk but `get_task` fails (manifest unreadable) | Falls through to `remove_task_dir_for_delete` + `remove_lock_only_task_dir`. Both probe both folders. Dir is removed; cascade runs. |
| APFS returns transient `Directory not empty` | Deletion retries three times with short backoff under the deletion fence. |
| A late task/execution/event writer wakes after delete | The external lifecycle lock + durable deletion marker rejects it; the removed directory stays absent. |
| Physical removal fails while a task directory survives | The durable marker is rolled back under the event lock; the original delete error is returned, external references are preserved, and the surviving task remains writable. |
| Published surface file removal fails | Logged; cascade continues. Surface row may show as "stale" in `/published-surfaces` until the index is next rewritten. |
| Feed projection removal fails | Logged; the projection's own periodic reconciliation eventually drops orphans. |

## Adding a new cascade target

When introducing a new artifact type that's keyed by `task_id` but lives outside `tasks/<id>/`, add a step to `cleanup_task_external_artifacts` rather than the generic `remove_task_dir_for_delete`. Keep new failures non-fatal with a `warn!` — the task dir is already gone by the time cascade runs, so erroring out would leave orphans without a retry path.
