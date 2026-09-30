# Recurring Monitors (web client)

Backend/component context: [recurring-monitors](../magician/recurring-monitors.md) ·
plan: Recurring Monitors productization

## Surface map

- **Route** — `/tasks?type=monitors` is a third tab on the tasks route shell
  (`src/routes/(app)/tasks/+page.svelte`), rendered by
  `src/lib/monitors/MonitorsWorkspace.svelte`. URL contract:
  `?type=monitors[&state=active|paused][&selected=task_…[&update=mu_…]][&compose=1]`.
  `monitorsTaskRoute(taskId?, updateId?)` in
  `src/lib/magician/tasks/taskRoutes.ts` is the one link builder; Today cards
  and Tauri notifications resolve through it to the exact update (`&update=`
  highlights it on the Updates tab).
- **API client** — `src/lib/monitors/api.ts`: `timedFetch` + scope-identity
  headers/query over `/api/magician/v3/monitors`
  (list/detail/create/patch/delete/pause/resume/run/runs/updates, scope-wide
  `/monitor-updates`). Errors surface the backend's stable `error` reasons.
- **Types** — `src/lib/types/monitor.ts` (`MonitorDetailV1`,
  `MonitorItemsPageV1`, `TaskScheduleWire` with serde external tagging
  `{"kind":{"Cron":{…}}}`, request/response bodies).

### Cursor pagination under the shared pager

`GET /monitors` answers `{items, next_cursor, limit, total, offset}`; `total`
is what a cursor cannot say (how many pages). The surface renders the shared
`ServerPager` (as the Tasks and Internal tabs do) with a page-size `Select`
(`[25, 50, 100, 250]`, default 25; changing it reloads from page one).

- **Movement is cursor, never arithmetic.** Next follows `next_cursor`;
  Previous/First reuse stacked cursors; Last walks the chain until the server
  stops issuing cursors. There is no `offset` request parameter; the response
  `offset` only renders `start-end`.
- **The cursor outranks the total on reachability**: a live cursor keeps one
  more page reachable if `total` under-reports; an exhausted cursor ends the
  count where the reader stands.
- **Absent `total` means cursor paging only, never zero pages**:
  `monitorPagerView` returns `null` and the surface falls back to
  accumulate-and-Load-more (older servers).
- **A mutation does not move the reader**: pause/resume/saved edit call
  `pager.reloadCurrentPage()`; `refresh()` is only Refresh and error Retry.
- **Deletion** follows the shared [paged-list removal](./paged-list-removal.md)
  policy via `pager.removeRow(taskId)` (drop row and its share of `total`,
  re-read, step back if empty; a failed re-read moves nobody).
- Built on `createSerializedCursorLoader` + `createGenerationGuard`
  (`$lib/attention/pagination`): concurrent forward loads coalesce, filter/size
  changes and refreshes invalidate late responses, the fallback dedupes by
  `task_id`. Filter/refresh/size changes clear to the skeleton; a page move keeps
  current rows until the new page lands and stays put on failure.

### List and detail

- **Rows** — native `Card` with state badge, health badge (`healthBadge`),
  server `cadence_summary` verbatim, last-scan status; `state` filter chips ride
  the server query. Fixed-size skeletons on first load, `EmptyState`, inline
  error + Retry.
- **Detail** — `MonitorDetailPanel.svelte`, four tabs: **Latest**, **Updates**
  (durable notification ledger; un-emitted records marked "Not notified"),
  **Runs** (every accepted `MonitorRunResultV1` incl. unchanged/degraded, with
  per-source outcomes and access problems), **Settings** (typed contract,
  revision, schedule, and an "Open task view" link to `/tasks?selected=` —
  execution inspection stays on the shared task panel). Actions: Run now /
  Pause / Resume / Edit / Delete (confirm via `confirmationStore`). Every write is
  guarded by a captured `thisTaskId`.
- **Composer** — `MonitorComposer.svelte`: objective, URLs, cadence preset or
  custom cron + timezone and notify policy always visible; domains/seeds/rules/
  match mode/signed-in sources/baseline-notify behind "More options". Submitting
  enters **review-before-activate** (normalized contract, human cadence, raw cron
  behind a toggle); POST/PATCH happens only from that step. Opened by the
  palette's **Create monitor** or `compose=1`. A closure-level in-flight lock
  prevents double-submit. Step containers take `tabindex="-1"` and receive focus
  on form↔review swaps.
- **Form logic** — `specForm.ts` mirrors the backend admission gate
  (`monitor_spec.rs::validate_and_normalize`) reason-for-reason
  (`monitor_objective_required`, `monitor_sources_required`,
  `monitor_source_url_scheme_unsupported`, …); `cadenceSummary` mirrors
  `monitors_api::cadence_summary` so review, rows and chat preview agree. The
  server stays authoritative.
- **Command palette** — `Open monitors` (Jump to), `Create monitor` (Actions →
  `/tasks?type=monitors&compose=1`).
- **Today** — `monitor_update` Changed cards get a `Monitor` source label and
  open `monitorsTaskRoute(metadata.monitor_task_id, metadata.update_id)`. Access
  problems use the existing escalation/attention rendering.

## Feedback UX

Every material update (a `changed` ledger record; baselines and quiet
`every_run` receipts excluded by `isMaterialUpdate`), on the Latest card and each
Updates row, offers:

1. **Useful** / **Not relevant** — `aria-pressed` chips POSTing
   `/monitors/{task_id}/updates/{update_id}/feedback` `{verdict}`. Optimistic,
   settled from the response (identical for an idempotent `recorded:false`
   replay, which still carries the authoritative verdict + `feedback_id`), rolled
   back into the action-error banner on failure. The opposite verdict replaces
   the stored one. Disabled per update while in flight.
2. **Edit monitor** — the composer in edit mode.
3. **Pause monitor** — shown only while active and scheduled.

Stored verdicts load with the detail (`GET /monitors/{task_id}/feedback`, merged
by `feedbackState.ts`: one verdict per `update_id`, newest `recorded_at` wins) as
a "Marked useful/not relevant" badge. The read is partial-tolerant (failure →
no stored verdicts). Feedback is evidence only; it never rewrites the contract
client-side. The wire accepts a `note ≤500` that has no web input.

## Convert a task to a monitor (Phase 7)

Eligible `/tasks` rows carry **Convert to monitor** in the `⋯` menu
(`TaskCardMenu`, `convert_monitor`). `canConvertTaskToMonitor`
(`src/lib/monitors/convert.ts`) requires a persistent lifecycle, a real mutable
task (`source === 'task'`, not read-only), and not already a monitor — V3 rows
carry the server-owned `monitor_revision` (`> 0` = monitor; omitted while 0),
mapped to `Task.monitorRevision`. The server re-checks on POST.

It opens `MonitorComposer` with `mode="convert"` in a Modal (`TasksWorkspace`),
prefilled by `convertFormFromTask` (objective ← description, else title; title ←
title). The cadence editor is replaced by a read-only "Keeps its current
schedule: …" (`keptScheduleSummary`) because the body is `{spec, title?}` only —
the task keeps its id, schedule, history, executions and outputs. The same
review step applies; **Convert to monitor** POSTs
`/monitors/{task_id}/convert` (`ConvertMonitor{Request,Response}V1`). Refusals
render via `specReasonLabel` (`monitor_already_exists`,
`task_not_eligible_for_monitor`, `task_not_found`, standard `monitor_*`).
Conversion is user-explicit only — no title heuristics. Limit:
`keptScheduleSummary` renders only cron schedules (the store's decoder drops
Interval/Once/OnEvent), so non-cron schedules display "unscheduled" while the
backend keeps them verbatim.

## Tests

`src/lib/types/monitorContract.test.ts` (cross-platform fixtures in
`magician/tests/fixtures/monitors/`), `src/lib/monitors/{convert,pagination,specForm,feedbackState}.test.ts`,
`src/lib/magician/tasks/taskRoutes.test.ts`. Run:
`npx vitest run src/lib/monitors src/lib/magician/tasks/taskRoutes.test.ts`.

Mobile web stays behind the mobile blocker by design; iOS is
[magios/monitors](../magios/monitors.md).
