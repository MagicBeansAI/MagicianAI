# Agent-created task dispatch (trust-tiered)

When an agent calls the `create_task` tool, the task is created **and dispatched at
creation** for the safe same-scope case — both dispatch loops are cron-gated, so an
unscheduled task would otherwise orphan in `ready`. Design (archived):
`docs/archive/plans/2026-07-12-agent-task-dispatch-trust-tiered-design.md`.

## Why dispatch belongs at creation

`ArtifactV2Service::start_execution` already runs a task by id from many call sites
(chat, the cron scheduler, the `run_task` handler). A `schedule=null` task is invisible
to the scheduled-task loop (`list_scheduled_tasks_across_scopes` skips tasks with no
cron/once schedule). A permanent
background "ready drainer" was rejected: it would auto-run duplicate recurring clones.
Dispatch belongs where intent and dedup live — at creation.

## Trust tier (for `create_task`)

`create_task` **locks ownership to the calling agent**, so its tasks are always
same-scope, same-owner, and flat. The disposition therefore reduces to run-now vs.
leave-ready (`decide_run_disposition`):

- **`Now`** (auto-dispatch via `start_execution`, mirroring `run_task`) when the task
  has no schedule, the caller did not pass `run: "manual"`, the kill switch is on, and
  the auto-dispatch depth is within cap.
- **`Manual`** (stays `ready`) otherwise. A scheduled task is left to the
  cron/one-time path; a
  depth-capped or kill-switched task is left for the operator / one-time reconciler.

The `create_task` result reports `disposition`: `dispatched` (+ `execution_id`),
`ready`, `dispatch_error` (dispatch failed but the task persists — recoverable), or
`deduplicated` (see below).

The genuinely cross-owner creation path is the harness `promote_backlog_item`, which
routes cross-owner promotions to an owner-approval lane (not covered by this handler).

## Task audience in autonomous harnesses

Harness execution records are implementation work, not user commitments, by
default. Harness `create_task` and `promote_backlog_item` therefore create
`lifecycle: Internal` tasks. Either tool may set `user_visible: true` only when
the output is a durable commitment the user is expected to manage in `/tasks`.
`reassign_task` preserves the original lifecycle, so changing an assignee never
changes the task's audience. Normal chat `create_task` behavior is unchanged and
remains user-visible when the user explicitly asks to create or track work.

## Guards

- **Kill switch** — `config::auto_dispatch_enabled()` (default ON; opt-out
  `MAGICIAN_AUTO_DISPATCH_CREATED_TASKS=0/false/no/off`). Off ⇒ every create stays
  `ready`.
- **Per-cycle spawn cap** — the pre-existing `__max_spawned_tasks` + provenance-tag
  counter bounds how many tasks one execution spawns (width). Unchanged.
- **Auto-dispatch depth cap** — bounds generations (depth). The created task carries an
  `auto-dispatch-depth:N` tag; `N` = parent-task depth + 1 (parent read from the source
  task's tag, default 0 for user/chat/scheduler origins). `N > AUTO_DISPATCH_MAX_DEPTH`
  (3) ⇒ created `ready`, not dispatched — the fork-bomb backstop the width cap can't
  give.
- **Concurrency reservation** — inherited from the StartNow admission path.

## Idempotent create (dedup)

Before creating, the handler dedups against **live** (non-terminal:
ready/running/pending/planning/paused) tasks owned by the **same agent** with the same
**canonical title** (trim + collapse whitespace + case-fold). On a hit it returns the
existing task id (`status: "deduplicated"`) instead of cloning — this prevents
recurring-cycle clone pile-up and is what makes auto-dispatch safe (no clones to
amplify). A completed same-title task does **not** block a new create. Opt out with a
present `schedule` or an explicit `allow_duplicate: true`. Dedup/depth listing is
best-effort and **fail-open**: a transient listing error proceeds with the create.

## One-time reconciler (existing orphans)

Tasks orphaned in `ready` without dispatch-at-creation are drained by an operator-triggered
one-time reconciler (`execution/task_reconcile.rs`). The preferred operator path is a
dry-run-first command against the running service:

```bash
magician reconcile-orphaned-tasks
magician reconcile-orphaned-tasks --apply
```

The CLI takes `--api-base` (default `http://127.0.0.1:3002`) and `--batch-cap`;
scope comes from the workspace-bound bearer in `MAGICIAN_BEARER_TOKEN`.

The same surface is available as
`POST /api/magician/v2/admin/tasks/reconcile-orphaned-ready`; requests default to
`dry_run: true`, require explicit scope, and reject batch caps outside `1..=100`.
For unattended one-time migration, `run_agent_startup_hydration` can also run it when
`MAGICIAN_RECONCILE_ORPHANED_TASKS_ON_BOOT` is set (`1`/`true`/`yes`/`on`, default
OFF). Per scope it lists tasks, then a
**pure planner** (`plan_reconcile`, unit-tested) selects which to dispatch:

- **Orphan candidate** = `status == ready` + `approved` + `schedule == null` +
  agent/harness origin (`created_by == "agentic_compiled"` or `system:harness:*`) — a
  *whitelist*, so user-facing `ready` tasks (`feed_insight` follow-ups, `observe`, …) are
  never swept — AND not carrying the `no-auto-dispatch` tag (a task created with explicit
  `run: "manual"` is deliberately held; the reconciler must not resurrect it). (Internal/
  delegation tasks live in a separate store off this listing; scheduled tasks are the cron
  path's.)
- **Depth-capped skip** — a task the dispatch-at-creation depth cap deliberately held
  (`auto-dispatch-depth > AUTO_DISPATCH_MAX_DEPTH`) is skipped, not resurrected (it would
  restart the fork-bomb backstop).
- **Durable dedup** by `(owning agent, canonical title)` — against any already-live
  instance AND within the orphan set. Duplicate task records are cancelled before the
  keeper dispatches. If retirement fails, that keeper is held and the error is reported,
  so later reconciler runs cannot execute historical clones one by one.
- **Plan-pending** (`has_plan && approved_plan_id.is_none()`) is left for operator
  review, never auto-dispatched.
- **Batch cap** (`DEFAULT_RECONCILE_BATCH_CAP` = 25) bounds a boot-time drain; a large
  backlog drains over successive re-runs.

It reuses the same `start_execution` and is gated by `auto_dispatch_enabled()`.
**Idempotent** — a re-run finds the keeper no longer `ready` and historical duplicates
terminally cancelled, so it is safe to leave armed. Counts and affected task ids
(scanned / dispatched / deduped / skipped / mutation errors) are returned and logged,
never silently truncated.

## Cross-owner promotion → owner lane (mode-3 fix)

A harness officer promoting a backlog item assigns the resulting task to an `agent_id`.
When that agent is **outside the promoting harness's scope**
(`HarnessScope::contains` is false), a hard error would make an *autonomous* harness run
dead-pause waiting for a human who never answers (`waiting_for_user`).

So the cross-owner case returns a **non-fatal** result (`owner_approval_required_result`)
instead of an error: the backlog item is **left `Proposed` in the shared company backlog**
and the promoting agent gets `{status: "owner_approval_required", requested_owner_agent,
requesting_officer, guidance: "…Do NOT pause for the user — continue your cycle…"}`. No
cross-scope task is created — the scope boundary holds — and the autonomous run keeps moving.

**Owner-directed routing.** The request is persisted and surfaced, not left to rot. The
cross-owner branch stamps `requested_owner_agent` / `requested_by_officer` on the item
(`BacklogStore::set_owner_request`, keeping it `Proposed` — no cross-scope task), and the
shared **"Company Backlog (open)"** block that `append_harness_program_context` renders into
*every* officer's harness cycle shows the flag (`[→ requested for <agent> by <officer>: if
<agent> is one of your agents, promote this item in your scope]`). So the owner whose scope
contains that agent sees the directed request and promotes it under its own authority. The
branch is **idempotent** — a re-attempt with the same request is a no-op that reports
`already_requested`, so an officer can't churn a re-promote loop.

`owner_approval_required_result` is a shared helper; the sibling cross-owner guards on
harness `create_task` / `create_proposal` still hard-error and can adopt it the same way.
