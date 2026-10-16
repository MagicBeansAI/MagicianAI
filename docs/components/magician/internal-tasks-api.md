# Internal Tasks API

REST endpoints for the Work page's internal view (`/work?type=internal`). They
expose tasks the regular `GET /v3/tasks` listing filters out: system seeds,
chat-spawned delegate transients, agent-cycle and harness work, debug-page runs,
contextual/extension automation, and default VibeDev cockpit runs. The legacy
`/internal-tasks` and `/debug/tasks` URLs redirect there and keep their other
query parameters (`ui/unified-ui/src/lib/work/routes.ts`).

Source of truth: `magician-api/src/task_api_v3.rs`. Routes are registered in
`magician-bin/src/main.rs` (`web::scope("/api/magician/v3")`).
The Unified UI client is `ui/unified-ui/src/lib/internalTasks/api.ts`; the route
delegates list/detail/delete/cancel/retry and output open/reveal/download to it.

### Creating an internal task

Internal tasks carry `lifecycle: Internal` and live in `internal_tasks/`. They
come from:

- chat-spawned delegations, sub-goals and handovers (`chat_session_id` set, so
  they are swept when the chat is cleared);
- debug-page runs, `POST /api/magician/v2/executions { debug: true }`
  (`created_by: __system__`);
- contextual/extension work such as iOS webpage assistance,
  `POST /api/magician/v2/executions { internal: true }`. Unlike `debug`, this
  keeps the user's provenance and has no `__system__` framing;
- autonomous harness cycle, autofix, create and backlog-promotion work. Harness
  `create_task` and `promote_backlog_item` stay Internal unless the model passes
  `user_visible: true` for a commitment the user should manage;
  `reassign_task` keeps the source lifecycle.

**VibeDev cockpit runs (intent-gated).** `POST /api/magician/v3/tasks` creates a
VibeDev run as `Internal` by default: it is a cockpit execution, not a tracked
deliverable. The handler gates on `is_vibedev_cockpit_run` (Build or Discuss;
broader than the build-only `is_vibedev_coding_build_run`) and switches to the
user-visible `Persistent` lifecycle only when the request carries
`save_as_task: true` (the composer's **"Save as task"** toggle). Runs keep
`chat_session_id: None`, so an Internal build is **not** swept on chat deletion;
it stays in `internal_tasks/` until removed. The cockpit's run-history rail reads
`GET /v3/tasks/internal?ui_thread_id=vibedev` and merges it with the persistent
`/tasks` feed.

## Scope identity

Every endpoint here requires a workspace-bound bearer. Principal and workspace
come from the `X-Principal` / `X-Workspace` headers the auth middleware writes
from that credential (`resolve_required_scope_ref`, `api_scope.rs`). A
`workspace` query parameter is accepted but ignored; there is no `principal`
parameter.

## `GET /api/magician/v3/tasks/internal`

Lists a scope's internal tasks with server-side paging, sort and filters; powers
the `/work?type=internal` table. The response is metadata only: it reads
manifests and state from `internal_tasks/` but does not summarize outputs, walk
plan history or resolve dependencies per row, so polling cannot hang on a large
historical run. Use the detail endpoint for deep inspection.

### Query parameters

| Name | Type | Default | Notes |
| --- | --- | --- | --- |
| `limit` | integer | `50` | Clamped to `[1, 500]`. |
| `offset` | integer | `0` | Ignored when `cursor` is set. |
| `cursor` | string | — | `pagination.next_cursor` from the previous page. Only valid with the default `updated_at`/`desc` order; otherwise `400 cursor_requires_updated_at_order`. An undecodable cursor is `400 invalid_task_cursor`. |
| `sort` | string | `updated_at` | `updated_at`, `created_at`, `title`, `agent_id` or `status`; anything else falls back to `updated_at`. |
| `order` | string | `desc` | `asc` or `desc`; anything else is `desc`. |
| `agent_id` | string | — | Exact match; empty is ignored. |
| `status` | string | — | Exact match; empty is ignored. `status=archived` alone implies `archived=only`. |
| `query` | string | — | Case-insensitive substring over `id`, `title`, `agent_id` and `status`. Empty is ignored. |
| `archived` | string | `exclude` | `exclude`, `include` or `only`; anything else is `400 invalid_archived_filter`. Archived tasks are not in the list index, so `include`/`only` use the file walk. `ui_thread_id=vibedev&archived=only` lists archived VibeDev runs. Same parameter and default on `GET /v3/tasks`. |
| `ui_thread_id` | string | — | Exact match on the task's `ui_thread_id`, so a surface pulls only its own runs (the VibeDev rail uses `?ui_thread_id=vibedev`). Empty is ignored. |

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
    "has_more": true,
    "next_cursor": "…"
  }
}
```

### Filter / sort semantics

A request with no filters, the default `updated_at`/`desc` order and
`archived=exclude` is served from the task list index. Any other request walks
`internal_tasks/<id>/` (`walk_internal_tasks`) and filters and sorts in memory.
The walk also includes app-workflow tasks whose directory is still under
`tasks/<id>/`; no other row under `tasks/<id>/` is shown in this view.
`next_cursor` is `null` on the last page and for non-default sorts.
`pagination.total` counts rows after filtering.

### No read-side recovery

The list does not reconcile: rows are the records as they are on disk. A stale
`active_root_execution_id` is checked against its execution when the task is
next planned or a new execution is provisioned for it
(`reconcile_stale_active_root_execution`, `artifact_v2/service.rs`).

### Errors

| Status | Body | Cause |
| --- | --- | --- |
| `400` | `{"error":"missing_scope", ...}` | No principal or workspace on the request. |
| `400` | `{"error":"invalid_archived_filter", ...}` | Bad `archived` value. |
| `400` | `{"error":"invalid_task_cursor", ...}` / `{"error":"cursor_requires_updated_at_order", ...}` | Bad `cursor`, or `cursor` with a non-default sort. |
| `500` | `{"error":"Failed to list internal tasks: <cause>"}` | Read-side IO/serde failure. |

## `GET /api/magician/v3/tasks/{task_id}/details`

One task with a page of executions, their outputs (with snippets) and persisted
artifacts; powers the Internal view's expandable rows and drawer. Default page
is 25 executions; `limit` is clamped to 1–100, `cursor` continues from
`next_execution_cursor`, and `execution_total` is the index count.

- Path: `task_id`, which must match the on-disk directory name.

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

`executions` is sorted newest first by `(started_at, execution_id)`. The cursor
holds the last key, so new executions do not shift older pages. `artifacts` is
best-effort: `[]` when `persisted_artifacts.json` is missing or unreadable. See
[Recurring App tasks](recurring-app-tasks.md) for scheduling and the owner
maintenance path.

### Errors

| Status | Body | Cause |
| --- | --- | --- |
| `400` | `{"error":"invalid request: execution_cursor_invalid"}` | `cursor` over 1024 bytes or not parseable. |
| `404` | `{"error":"Task not found"}` | Task dir missing in both `tasks/` and `internal_tasks/`. |
| `500` | `{"error":"Failed to load task details: <cause>"}` | IO/serde failure on the task or any execution. |

## `DELETE /api/magician/v3/tasks/internal/{task_id}`

Permanently removes a task and its lineage (the Delete button on
`/work?type=internal`). Like `DELETE /api/magician/v3/tasks/{id}?remove_files=true`
for user-visible tasks, but it also covers `internal_tasks/<id>/`.

- Path: `task_id`, validated by `ArtifactV2Workspace::validate_task_id` before
  anything is removed.

What is deleted:

1. The task directory wherever it lives (`tasks/<id>/` and/or
   `internal_tasks/<id>/`; both are probed and both removed).
2. Published surfaces with `record.task_id == Some(task_id)`: their
   `published_surfaces/<surface_id>.json` file and materialized muij layout, and
   their entries in the `published_surfaces.json` index.
3. The task's progress-channel log,
   `progress_channels/events/task:<id>.jsonl` under the scope root.
4. The task's feed projection summary and its task-index rows.

Not deleted: agent episode records (`memory/agent_episodes/…`, kept by design),
LLM observability logs (separate retention) and chat-pack execution dirs
(cleaned by the chat-session lifecycle). See `task-lineage-cleanup.md`.

### Response

```json
{
  "deleted": true,
  "task_id": "task_…"
}
```

The operation is idempotent: `deleted: true` is returned even if the task did
not exist.

### Errors

| Status | Body | Cause |
| --- | --- | --- |
| `409` | `{"error":"app_workflow_generic_lifecycle_denied", ...}` | The id is a governed app-workflow run; use app run-control instead. |
| `500` | `{"error":"Failed to delete internal task: invalid request: invalid_task_id:…"}` | The id failed validation (the handler maps it to 500, not 400). |
| `500` | `{"error":"Failed to delete internal task: <cause>"}` | Filesystem error removing the task dirs. |

Cascade failures (progress log, published-surface record or layout, feed
projection) are logged as `warn!` and do not fail the delete; the task dir is
already gone (`cleanup_task_external_artifacts`).

## Stop / cancel an execution

The Stop button uses the existing cancel route:

```
POST /api/magician/v3/executions/{execution_id}/cancel
```

`execution_id` comes only from the task's `active_root_execution_id`;
`latest_root_execution_id` is for history and is never a mutation target. Stop
goes through the shared per-execution control coordinator, so duplicate controls
cannot race it.

## `GET /api/magician/v3/tasks/{task_id}/outputs`

The task's stored outputs, classified for display. Handler
`get_task_outputs_v3_handler` → `ArtifactV2Service::get_task_outputs_view`.

```json
{
  "outputs": {
    "task_id": "task-1",
    "default_task_agent_output_id": null,
    "primary_user_output_id": "out_task_user_task-1",
    "outputs": [
      {
        "output_id": "out_task_user_task-1",
        "scope": "task",
        "audience": "user",
        "role": "primary_task_user",
        "relative_path": "outputs/out_task_user_task-1.html",
        "media_type": "text/html",
        "created_at": "2026-10-01T10:00:00Z",
        "source_execution_id": "exec-1",
        "source_plan_id": "plan-1",
        "source_output_ids": ["out_exec_exec-1"],
        "deliverable": true,
        "absolute_path": "/Users/me/MagicianNotes/.../tasks/task-1/outputs/out_task_user_task-1.html"
      },
      {
        "output_id": "out_task_continuation_task-1",
        "audience": "agent",
        "role": "continuation_context",
        "relative_path": "outputs/out_task_continuation_task-1.json",
        "deliverable": false,
        "absolute_path": "/Users/me/MagicianNotes/.../tasks/task-1/outputs/out_task_continuation_task-1.json"
      }
    ],
    "internal_files": [
      {
        "kind": "task_summary",
        "file_name": "task_summary.json",
        "absolute_path": "/Users/me/MagicianNotes/.../tasks/task-1/outputs/task_summary.json"
      }
    ]
  }
}
```

(The second entry is abbreviated; every entry carries all `OutputRef` keys.)

Each entry keeps its stored `OutputRef` keys and adds two:

- **`deliverable`** (`models::is_deliverable_output`): true for
  `audience == "user"` with `role ∈ {primary_task_user, user_media}`, the same
  rule Portable Export uses. Everything else is internal (the execution output,
  the continuation context used to resume the task, legacy
  `primary_task_agent` refs) and belongs under technical details.
- **`absolute_path`**: the canonical path, set only when `relative_path` names an
  existing regular file inside the task's `outputs/` directory. It is `null` when
  the file is missing, resolves outside `outputs/` (including via a symlink), or
  `relative_path` is absolute or has a `..`/`.` component. The drawer copies it
  when you click an output name.

**`internal_files`** lists sidecars that exist on disk (`task_summary.json`,
`voice_speech.json`); they are never deliverables.
**`default_task_agent_output_id`** is deprecated and always `null`.

## `POST /api/magician/v3/tasks/{task_id}/outputs/open-file`

Hands a task output to the OS default opener for its MIME type (the Open button
in the Internal output list).

```json
{ "relative_path": "outputs/report.html" }
```

`relative_path` takes two forms:

- **Task-level**: `outputs/<filename>` or bare `<filename>` (legacy), resolved
  against `task_outputs_dir`.
- **Execution-level**: `executions/<exec_id>/outputs/<filename>`, resolved
  against the task root to `<task_root>/executions/<exec_id>/outputs/<filename>`.

The canonicalized path must stay inside its security root
(`resolve_task_output_absolute_path`). This route has no extension blocklist:
the executable-extension check (`is_unsafe_to_auto_open`, `chat_api.rs`) guards
only the chat-session open-file route.

Response: `{ "file_path": "/abs/path/to/file" }`

| Status | Cause |
|---|---|
| 400 | `task_id` contains `..`, `/` or `\`, or the body has no `relative_path`. |
| 403 | Resolved path is outside the security root (`Path traversal denied`). |
| 404 | Task, outputs directory or file not found. |
| 500 | OS open command failed. |

## `POST /api/magician/v3/tasks/{task_id}/outputs/open-folder`

Reveals a task output in the OS file manager (Finder on macOS); the Reveal
button. Same body, path forms (`outputs/<file>` or
`executions/<exec_id>/outputs/<file>`) and security rules as `open-file`.

```json
{ "relative_path": "outputs/report.html" }
```

Response: `{ "folder_path": "/abs/path/to/folder", "file_path": "/abs/path/to/file" }`

## `GET /api/magician/v3/tasks/{task_id}/export`

Bundles a task's **user-facing** outputs into one self-contained artifact (the
deep panel's **Export ▾**). Handler `export_task_outputs_v3_handler`
(`api/task_api_v3.rs`).

| Param | Required | Notes |
|---|---|---|
| `format` | no | `zip` (default) or `single-html`. |
| `principal` / `workspace` | scope | Same scope resolution as the output-download route. |

- **`zip`**: root `index.html` is the primary HTML deliverable with its
  `/api/.../outputs/…` media URLs rewritten to `outputs/{path}`, plus every
  shareable output under `outputs/`. It opens standalone after unzip. Without an
  HTML primary, a minimal launcher `index.html` links each output.
- **`single-html`**: the primary HTML with each referenced media inlined as a
  `data:` URI (8 MiB per asset; oversize or missing assets keep their live URL).
  Returns **422** without an HTML primary.

Only deliverables are included (`models::is_deliverable_output`, as above);
internal outputs (`continuation_context`, legacy `primary_task_agent`) never
leave. Scope, auth and path-traversal safety mirror the output-download route;
`task_outputs_dir` works for tasks under `internal_tasks/` or `tasks/`. The
response has `Content-Disposition: attachment` (`task-{id}.zip` / `.html`).

| Status | When |
|---|---|
| 400 | Invalid `task_id`. |
| 404 | Task or outputs dir not found, or no shareable outputs. |
| 422 | `format=single-html` but no HTML primary deliverable. |
