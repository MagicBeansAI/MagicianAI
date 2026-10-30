# Agent-created task dispatch (trust-tiered)

When an agent calls the `create_task` tool, the task is created **and dispatched
at creation** in the safe same-scope case. Both dispatch loops are cron-gated, so
an unscheduled task would otherwise sit orphaned in `ready`. Design (archived):
`docs/archive/plans/2026-07-12-agent-task-dispatch-trust-tiered-design.md`.

## Why dispatch belongs at creation

`ArtifactV2Service::start_execution` already runs a task by id from many call
sites (chat, the cron scheduler, the `run_task` handler). A `schedule=null` task
is invisible to the scheduled-task loop (`list_scheduled_tasks_across_scopes`
skips tasks with no cron/once schedule). A permanent background "ready drainer" was
rejected because it would auto-run duplicate recurring clones, so dispatch
happens where intent and dedup live: at creation.

## Trust tier (for `create_task`)

`create_task` **locks ownership to the calling agent**, so its tasks are always
same-scope, same-owner and flat. The disposition reduces to run-now vs.
leave-ready (`decide_run_disposition`):

- **`Now`** (dispatch via `start_execution`, like `run_task`) when the task has
  no schedule, the caller did not pass `run: "manual"`, the kill switch is on
  and the auto-dispatch depth is within cap.
- **`Manual`** (stays `ready`) otherwise. A scheduled task is left to the
  cron/one-time path; a depth-capped or kill-switched task is left for the
  operator or the one-time reconciler.

The `create_task` result reports `disposition`: `dispatched` (+ `execution_id`),
`ready`, or `dispatch_error` (dispatch failed but the task persists and can be
run later). A dedup hit returns `status: "deduplicated"` instead (see below).

Task create/run/list/details and harness promotion results provide `task_url`
through `ui_links`: a Work link retaining internal-task identity and, when started,
the exact execution. Agents should use it instead of constructing UI paths.

The real cross-owner creation path is the harness `promote_backlog_item`, which
routes cross-owner promotions to an owner-approval lane (not this handler).

## Task audience in autonomous harnesses

Harness execution records are implementation work, not user commitments, so
harness `create_task` and `promote_backlog_item` create `lifecycle: Internal`
tasks. Either tool may set `user_visible: true` only when the output is a
durable commitment the user is expected to manage in `/work`. `reassign_task`
keeps the original lifecycle, so changing an assignee never changes the
audience. Normal chat `create_task` stays user-visible when the user asks to
create or track work.

## Guards

- **Kill switch**: `config::auto_dispatch_enabled()` (default ON; opt out with
  `MAGICIAN_AUTO_DISPATCH_CREATED_TASKS=0/false/no/off`). Off ⇒ every create
  stays `ready`.
- **Per-cycle spawn cap**: the `__max_spawned_tasks` + provenance-tag counter
  bounds how many tasks one execution spawns (width).
- **Auto-dispatch depth cap** bounds generations (depth). The created task
  carries an `auto-dispatch-depth:N` tag, where `N` = parent-task depth + 1
  (read from the source task's tag; 0 for user, chat and scheduler origins,
  and when the parent is not in the scope's task listing).
  `N > AUTO_DISPATCH_MAX_DEPTH` (3) ⇒ created `ready`, not dispatched: the
  fork-bomb backstop the width cap cannot give.
- **Concurrency reservation**: inherited from the StartNow admission path.

## Idempotent create (dedup)

Before creating, the handler dedups against **live** (any status except
`completed`/`failed`/`cancelled`) tasks owned by the **same agent** with
the same **canonical title** (trim + collapse whitespace + case-fold). On a hit
it returns the existing task id (`status: "deduplicated"`) instead of cloning.
This stops recurring-cycle clone pile-up and is what makes auto-dispatch safe. A
completed same-title task does **not** block a new create. Dedup is skipped
for a task with a `schedule`, with `allow_duplicate: true`, or when the kill
switch is off. Dedup and depth share one task listing that **fails open**: a
listing error logs a warning and the create proceeds without dedup, at
depth 1.

## One-time reconciler (existing orphans)

Tasks orphaned in `ready` are drained by an operator-triggered one-time
reconciler (`execution/task_reconcile.rs`). The preferred path is a dry-run-first
command against the running service:

```bash
magician reconcile-orphaned-tasks
magician reconcile-orphaned-tasks --apply
```

The CLI takes `--api-base` (default `http://127.0.0.1:3002`) and `--batch-cap`;
scope comes from the workspace-bound bearer in `MAGICIAN_BEARER_TOKEN`.

The same surface is `POST /api/magician/v2/admin/tasks/reconcile-orphaned-ready`:
requests default to `dry_run: true`, need explicit scope, and reject batch caps
outside `1..=100`. For an unattended one-time migration,
`run_agent_startup_hydration` runs it when
`MAGICIAN_RECONCILE_ORPHANED_TASKS_ON_BOOT` is set (`1`/`true`/`yes`/`on`,
default OFF). Per scope it lists tasks, then a **pure planner**
(`plan_reconcile`, unit-tested) picks what to dispatch:

- **Orphan candidate** = `status == ready` + `approved` + `schedule == null` +
  agent/harness origin (`created_by == "agentic_compiled"` or
  `system:harness:*`), and no `no-auto-dispatch` tag. The origin check is a
  whitelist, so user-facing `ready` tasks (`feed_insight` follow-ups,
  `observe`, …) are never swept; the tag marks a task created with explicit
  `run: "manual"`, which stays held. (Internal and delegation tasks live in a
  separate store; scheduled tasks belong to the cron path.)
- **Depth-capped skip**: a task the depth cap held
  (`auto-dispatch-depth > AUTO_DISPATCH_MAX_DEPTH`) is skipped, not
  resurrected.
- **Durable dedup** by `(owning agent, canonical title)`, against any live
  instance and within the orphan set. Duplicates are cancelled before the keeper
  dispatches. If retirement fails, that keeper is held and the error reported,
  so later runs cannot execute old clones one by one.
- **Plan-pending** (`has_plan && approved_plan_id.is_none()`) is left for
  operator review, never auto-dispatched.
- **Batch cap** (`DEFAULT_RECONCILE_BATCH_CAP` = 25) bounds a boot-time drain;
  a large backlog drains over successive runs.

It reuses `start_execution` and is gated by `auto_dispatch_enabled()`. It is
**idempotent**: a re-run finds the keeper no longer `ready` and duplicates
cancelled, so it is safe to leave armed. Counts and affected task ids (scanned /
dispatched / deduped / skipped / mutation errors) are returned and logged, never
silently truncated.

## Cross-owner promotion → owner lane

A harness officer promoting a backlog item assigns the task to an `agent_id`.
When that agent is **outside the promoting harness's scope**
(`HarnessScope::contains` is false), a hard error would leave an *autonomous*
harness run paused in `waiting_for_user` for a human who never answers.

So the cross-owner case returns a **non-fatal** result
(`owner_approval_required_result`): the backlog item stays **`Proposed` in the
shared company backlog**, and the promoting agent gets
`{status: "owner_approval_required", requested_owner_agent, requesting_officer,
guidance: "…Do NOT pause for the user — continue your cycle…"}`. No cross-scope
task is created and the autonomous run keeps moving.

**Owner-directed routing.** The branch stamps `requested_owner_agent` /
`requested_by_officer` on the item (`BacklogStore::set_owner_request`, still
`Proposed`). The shared **"Company Backlog (open)"** block that
`append_harness_program_context` renders into *every* officer's harness cycle
shows the flag (`[→ requested for <agent> by <officer>: if <agent> is one of
your agents, promote this item in your scope]`), so the owner whose scope holds
that agent promotes it under its own authority. A repeat of the same request is
a no-op that reports `already_requested`, so an officer cannot churn a
re-promote loop.

`owner_approval_required_result` is a shared helper; the sibling cross-owner
guards on harness `create_task` / `create_proposal` still hard-error and can
adopt it the same way.
