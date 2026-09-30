# Internal Tasks API

REST endpoints for the `/tasks?type=internal` view. They expose non-user-visible tasks (system seeds, chat-spawned delegate transients, agent-cycle and harness work, debug-page runs, contextual/extension automation, and default VibeDev cockpit runs) that the regular `/tasks` listing filters out. The legacy `/internal-tasks` URL redirects to this canonical view and preserves selection/filter query parameters.

Source of truth: `magician/src/magician_v2/api/task_api_v3.rs`. Route registration: `magician/src/bin/magician.rs` (`web::scope("/api/magician/v3")`). The Unified UI client contract lives in `ui/unified-ui/src/lib/internalTasks/api.ts`; the route delegates list/detail/delete/cancel/retry plus output open/reveal/download serialization to that tested module.

### Creating an internal task

Internal tasks all carry `lifecycle: Internal` (routed to `internal_tasks/`). Creation paths include chat-spawned delegations/sub-goals/handovers (`chat_session_id` set → auto-swept on chat clear); debug-page runs via `POST /api/magician/v2/executions { debug: true }` (`created_by: __system__`); contextual/extension work such as iOS webpage assistance via `POST /api/magician/v2/executions { internal: true }` (user provenance preserved); and autonomous harness cycle, autofix, create, or backlog-promotion work. The `internal` flag is distinct from `debug`: it gives Internal routing without the `__system__` debug framing. Harness `create_task` and `promote_backlog_item` remain Internal unless the model uses the explicit `user_visible: true` parameter for a commitment the user is expected to manage; `reassign_task` preserves the source lifecycle.

**VibeDev cockpit runs (intent-gated).** `POST /api/magician/v3/tasks` creates a VibeDev run `Internal` by **default** — a cockpit-coupled execution, not a tracked deliverable, so it stays off the `/tasks` feed. The create handler gates on `is_vibedev_cockpit_run` (Build OR Discuss; broader than the build-only `is_vibedev_coding_build_run`) and flips to the user-visible `Persistent` lifecycle only when the request carries `save_as_task: true` (the composer's **"Save as task"** toggle). Runs keep `chat_session_id: None`, so an Internal build is **not** auto-swept on chat deletion — it persists in `internal_tasks/` until explicitly removed. The cockpit's run-history rail still shows Internal runs by reading `GET /v3/tasks/internal?ui_thread_id=vibedev` and merging that with the persistent `/tasks` feed (so `/tasks` stays clean while the rail is complete).

## Scope identity

All three endpoints require a workspace-bound bearer. Principal/workspace are
resolved from that credential and are not caller-selected request fields.

## `GET /api/magician/v3/tasks/internal`

List internal tasks for a scope with server-side pagination, sort, and filtering. Powers the `/tasks?type=internal` data table.

The list response is intentionally metadata-only: it reads manifests/state from
canonical `internal_tasks/` storage but does not summarize outputs, walk plan
history, or resolve dependencies per row, so polling surfaces cannot hang on a
large historical run. Use the detail endpoint for deep inspection.

### Query parameters

| Name | Type | Default | Notes |
| --- | --- | --- | --- |
| `principal` | string | required (or via header) | |
| `workspace` | string | required (or via header) | |
| `limit` | integer | `50` | Clamped to `[1, 500]`. |
| `offset` | integer | `0` | |
| `sort` | string | `updated_at` | One of `updated_at`, `created_at`, `title`, `agent_id`, `status`. Anything else falls back to `updated_at`. |
| `order` | string | `desc` | `asc` or `desc`. Anything else is treated as `desc`. |
| `agent_id` | string | — | Exact-match filter; empty string is ignored. |
| `status` | string | — | Exact-match filter; empty string is ignored. |
| `query` | string | — | Case-insensitive substring search across `id`, `title`, `agent_id`, and `status` combined. Empty string is ignored. |
| `ui_thread_id` | string | — | Exact-match filter on the task's `ui_thread_id`. Lets a surface pull only its OWN internal runs — e.g. the VibeDev cockpit run-history rail fetches `?ui_thread_id=vibedev` instead of the whole internal pool. Empty string is ignored. |

### Response

```json
{
  "tasks": [
    {
      "id": "task_…",
      "title": "…",
      "description": "…",
      "agent_id": "…",
      "status": "completed",
      "priority": null,
      "created_at": "2026-05-21T13:00:00Z",
      "updated_at": "2026-05-21T13:05:00Z",
      "created_by": "chat_inline",
      "active_root_execution_id": null,
      "latest_root_execution_id": "exec_…",
      "last_completed_root_execution_id": "exec_…",
      "...": "remaining TaskListItemV3 fields"
    }
  ],
  "pagination": {
    "total": 173,
    "limit": 50,
    "offset": 0,
    "has_more": true
  }
}
```

### Filter / sort semantics

Filtering and sorting are applied in-memory after `service.list_internal_tasks` reads the canonical `internal_tasks/<id>/` storage root. Legacy/internal-looking rows that still live under `tasks/<id>/` are intentionally not surfaced in the Internal view. If a stale duplicate root exists, path resolution prefers the canonical `internal_tasks/<id>/` record. Execution workspace setup must attach to an existing task root and should not create an empty `tasks/<id>/` sibling for internal tasks. `total` in the pagination block is the count after filtering, not the raw count for the scope.

### Read-side recovery

The list path also performs bounded reconciliation before rendering rows. If an
internal task still points at an active root execution, the service checks the
execution state and canonical event log. A terminal
`execution.outcome_observed` row wins over stale `running` /
`waiting_for_user` state: the terminal reducer is replayed, task/execution
refs and progress projections are refreshed, and the active root pointer is
cleared before the row is returned. This keeps cancelled/stopped internal work
from lingering as running after a process interruption during finalization.

Agentic `hitl.requested` rows are also projected as pending requested-input
attention summaries while the execution is `waiting_for_user`, including
max-iteration prompts. A paused internal task should therefore either show an
actionable attention item or recover to a terminal state during the same list
read.

### Errors

| Status | Body | Cause |
| --- | --- | --- |
| `400` | `{"error":"scope_required"}` (via `resolve_required_scope_ref`) | Missing principal/workspace. |
| `500` | `{"error":"Failed to list internal tasks: <cause>"}` | Read-side IO/serde failure. |

## `GET /api/magician/v3/tasks/{task_id}/details`

Enriched single-task view: the task record plus a page of executions, with outputs (snippets included) and persisted artifacts. The default page holds 25 executions; `limit` accepts 1–100 and `cursor` continues from `next_execution_cursor`. `execution_total` reports the index count. Powers the Internal view's expandable rows and task drawer.

### Path parameters

- `task_id` — the task id. Must match the on-disk dir name.

### Query parameters

- `principal`, `workspace` (or headers) — required.

### Response

```json
{
  "task": {
    "manifest": { "...": "TaskManifest" },
    "state":    { "...": "TaskState" },
    "refs":     { "...": "TaskRefs" }
  },
  "executions": [
    {
      "state":  { "...": "ExecutionState" },
      "refs":   { "outputs": [/* PersistedOutput */], "artifacts": [], "...": "remaining ExecutionRefs" },
      "artifacts": [
        /* raw persisted_artifacts.json entries */
      ]
    }
  ]
}
```

`executions` is sorted newest first by `(started_at, execution_id)`. The cursor retains the last key, so newly arriving executions do not shift older pages. See [Recurring App tasks](recurring-app-tasks.md) for scheduling and the separate owner maintenance path.

`artifacts` is best-effort: if `persisted_artifacts.json` is missing or unreadable, the field is `[]` and the rest of the execution still renders.

### Errors

| Status | Body | Cause |
| --- | --- | --- |
| `404` | `{"error":"Task not found"}` | Task dir missing in both `tasks/` and `internal_tasks/`. |
| `500` | `{"error":"Failed to load task details: <cause>"}` | IO/serde failure on the task record or any execution. |

## `DELETE /api/magician/v3/tasks/internal/{task_id}`

Permanently removes a task and its lineage. Used by the Delete button on `/tasks?type=internal`. Equivalent to `DELETE /api/magician/v3/tasks/{id}?remove_files=true` for user-visible tasks but works for tasks living in `internal_tasks/<id>/` too.

### Path parameters

- `task_id` — validated against path-traversal patterns (`.`, `..`, `/`, `\`). Invalid ids return `400`.

### Query parameters

- `principal`, `workspace` (or headers) — required.

### What gets deleted

1. The task directory in whichever folder it lives (`tasks/<id>/` and/or `internal_tasks/<id>/`). Both are probed; if both exist they're both removed.
2. Published surfaces with `record.task_id == Some(task_id)` — their per-surface JSON file under `published_surfaces/<surface_id>.json` and the materialized muij layout. The `published_surfaces.json` index is rewritten to drop the entries.
3. The feed projection summary for the task.

What is **not** deleted: agent episode records (`memory/agent_episodes/…` survive task deletion by design), LLM observability logs (separate retention), and chat-pack execution dirs (cleaned by chat-session lifecycle). See `task-lineage-cleanup.md` for the rationale.

### Response

```json
{
  "deleted": true,
  "task_id": "task_…"
}
```

`deleted: true` is returned even if the task didn't exist — the operation is idempotent.

### Errors

| Status | Body | Cause |
| --- | --- | --- |
| `400` | `{"error":"invalid_task_id:…"}` | Path-traversal validation rejected the id. |
| `500` | `{"error":"Failed to delete internal task: <cause>"}` | Filesystem error while removing dirs or cascading external artifacts. |

Cascade failures (a published-surface file that can't be unlinked, a feed-projection adapter that errors) are logged as `warn!` but do not fail the delete — the task dir is already gone by that point.

## Stop / cancel an execution

The Internal view's Stop button doesn't have its own endpoint; it dispatches to the existing execution-cancel route:

```
POST /api/magician/v3/executions/{execution_id}/cancel
```

`execution_id` is taken only from the task's `active_root_execution_id`.
`latest_root_execution_id` remains available for history/inspection and is
never used as a mutation target. The Stop request participates in the shared
per-execution control coordinator, so duplicate controls cannot race it.

## `POST /api/magician/v3/tasks/{task_id}/outputs/open-file`

Hand a task output file to the OS default opener (Preview, Word, VLC, browser — whatever is registered for the MIME type). Used by the Open button in the Internal task output list.

### Request body

```json
{ "relative_path": "outputs/report.html" }
```

`relative_path` accepts two forms:

- **Task-level outputs** — `outputs/<filename>` or bare `<filename>` (legacy). Both resolve relative to `task_outputs_dir` (the task's `outputs/` subdirectory).
- **Execution-level outputs** — `executions/<exec_id>/outputs/<filename>`. The `executions/` prefix is detected and the path is resolved relative to the task root directory instead, so the file lands at `<task_root>/executions/<exec_id>/outputs/<filename>`.

Path-traversal is rejected in both cases (canonicalized path must stay within the resolved security root).

Executable extensions (`.app`, `.sh`, `.exe`, etc.) are blocked — use `open-folder` and open manually.

### Response

```json
{ "file_path": "/abs/path/to/file" }
```

### Errors

| Status | Cause |
|---|---|
| 400 | Invalid `task_id` or no `relative_path` supplied. |
| 403 | Path traversal detected, or unsafe-to-auto-open extension. |
| 404 | Task not found, outputs directory missing, or file not found. |
| 500 | OS open command failed. |

## `POST /api/magician/v3/tasks/{task_id}/outputs/open-folder`

Reveal a task output file in the OS file manager (Finder on macOS). Same path resolution and security rules as `open-file`. Used by the Reveal button in the Internal task output list.

### Request body

```json
{ "relative_path": "outputs/report.html" }
```

Accepts both task-level (`outputs/<file>`) and execution-level (`executions/<exec_id>/outputs/<file>`) paths.

### Response

```json
{ "folder_path": "/abs/path/to/folder", "file_path": "/abs/path/to/file" }
```

## `GET /api/magician/v3/tasks/{task_id}/export`

Bundle a task's **user-facing** outputs into a single shareable, self-contained artifact (the deep panel's **Export ▾** control). Handler `export_task_outputs_v3_handler` (`api/task_api_v3.rs`).

### Query parameters

| Param | Required | Notes |
|---|---|---|
| `format` | no | `zip` (default) or `single-html`. |
| `principal` / `workspace` | scope | Same scope resolution as the output-download route. |

### Behavior

- **`zip`** — a ZIP whose root `index.html` is the primary HTML deliverable with its `/api/.../outputs/…` media URLs rewritten to bundle-relative `outputs/{path}`, plus every shareable output under `outputs/`. Opens standalone after unzip. If the task has no HTML primary, a minimal launcher `index.html` linking each output is generated instead.
- **`single-html`** — the primary HTML with each referenced media inlined as a `data:` URI (8 MiB/asset cap; oversize/missing assets keep their live URL). One file, no unzip. Returns **422** when the task has no HTML primary.

Only `audience == "user"` outputs (`role ∈ {primary_task_user, user_media}`) are included — agent-only JSON (`primary_task_agent`, `continuation_context`) never leaves. Scope/auth and path-traversal safety mirror the output-download route; resolution uses `task_outputs_dir` (works for tasks under `internal_tasks/` **or** `tasks/`). Responds with `Content-Disposition: attachment` (`task-{id}.zip` / `.html`).

### Errors

| Status | When |
|---|---|
| 400 | Invalid `task_id`. |
| 404 | Task / outputs dir not found, or no shareable outputs. |
| 422 | `format=single-html` but no HTML primary deliverable. |
