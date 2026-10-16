# Recurring Monitors (web client)

Backend/component context: [recurring-monitors](../magician/recurring-monitors.md) ·
plan: Recurring Monitors productization

## Surface map

- **Route**: `/work?type=monitors` is a tab of the Work route
  (`src/lib/work/WorkWorkspace.svelte`), rendered by
  `src/lib/monitors/MonitorsWorkspace.svelte`. URL contract:
  `?type=monitors[&state=active|paused][&selected=task_…[&update=mu_…]][&compose=1]`.
  `monitorsTaskRoute(taskId?, updateId?)` in
  `src/lib/magician/tasks/taskRoutes.ts` is the one link builder; Today cards,
  Tauri notifications and monitor-tool `manage_url` values resolve through it
  to the exact update (`&update=` highlights it on the Updates tab). The legacy
  `/tasks` web host and its redirect are gone; native link parsers and saved
  in-app link normalization still accept older payloads.
- **API client**: `src/lib/monitors/api.ts`, `timedFetch` plus scope-identity
  headers and query over `/api/magician/v3/monitors`
  (list/detail/create/patch/delete/pause/resume/run/runs/updates, scope-wide
  `/monitor-updates`). Errors show the backend's stable `error` reasons.
- **Types**: `src/lib/types/monitor.ts` (`MonitorDetailV1`,
  `MonitorItemsPageV1`, `TaskScheduleWire` with serde external tagging
  `{"kind":{"Cron":{…}}}`, request and response bodies).

### Cursor pagination under the shared pager

`GET /monitors` answers `{items, next_cursor, limit, total, offset}`; `total`
says how many pages, which a cursor cannot. The surface renders the shared
`ServerPager` with a page-size `Select` (`[25, 50, 100, 250]`, default 25;
changing it reloads from page one).

- **Movement is cursor, never arithmetic.** Next follows `next_cursor`;
  Previous and First reuse stacked cursors; Last walks the chain until the
  server stops issuing cursors. There is no `offset` request parameter; the
  response `offset` only renders `start-end`.
- **The cursor outranks the total**: a live cursor keeps one more page
  reachable if `total` under-reports; an exhausted cursor ends the count.
- **Absent `total` means cursor paging only, never zero pages**:
  `monitorPagerView` returns `null` and the surface falls back to
  accumulate-and-Load-more (older servers).
- **A mutation does not move the reader**: pause, resume and saved edits call
  `pager.reloadCurrentPage()`; `refresh()` is only Refresh and error Retry.
- **Deletion** follows the shared [paged-list removal](./paged-list-removal.md)
  policy via `pager.removeRow(taskId)`.
- Built on `createSerializedCursorLoader` + `createGenerationGuard`
  (`$lib/attention/pagination`): concurrent forward loads coalesce, filter and
  size changes invalidate late responses, the fallback dedupes by `task_id`.
  Filter, refresh and size changes clear to the skeleton; a page move keeps
  rows until the new page lands and stays put on failure.

### List and detail

- **Rows**: native `Card` with state badge, health badge (`healthBadge`),
  server `cadence_summary` verbatim and last-scan status; `state` filter chips
  ride the server query. Skeletons on first load, `EmptyState`, inline error
  with Retry.
- **Detail**: `MonitorDetailPanel.svelte`, four tabs: **Latest**, **Updates**
  (durable notification ledger; un-emitted records marked "Not notified"),
  **Runs** (every accepted `MonitorRunResultV1`, including unchanged and
  degraded, with per-source outcomes and access problems), **Settings** (typed
  contract, revision, schedule, and an "Open task view" link to the shared task
  panel). Actions: Run now / Pause / Resume / Edit / Delete (confirm via
  `confirmationStore`). Every write is guarded by a captured `thisTaskId`.
- **Composer**: `MonitorComposer.svelte`. Objective, URLs, cadence preset or
  custom cron with timezone, and notify policy are always visible; domains,
  seeds, rules, match mode, signed-in sources and baseline-notify sit behind
  "More options". Submitting enters **review-before-activate** (normalized
  contract, human cadence, raw cron behind a toggle); POST/PATCH happens only
  from that step. Opened by the palette's **Create monitor** or `compose=1`.
  An in-flight lock prevents double-submit; step containers take
  `tabindex="-1"` and receive focus on form↔review swaps.
- **Form logic**: `specForm.ts` mirrors the backend admission gate
  (`monitor_spec.rs::validate_and_normalize`) reason for reason
  (`monitor_objective_required`, `monitor_sources_required`,
  `monitor_source_url_scheme_unsupported`, …); `cadenceSummary` mirrors
  `monitors_api::cadence_summary` so review, rows and chat preview agree. The
  server stays authoritative.
- **Command palette**: `Open monitors` (Jump to), `Create monitor` (Actions →
  `/work?type=monitors&compose=1`).
- **Today**: `monitor_update` Changed cards get a `Monitor` source label and
  open `monitorsTaskRoute(metadata.monitor_task_id, metadata.update_id)`.
  Access problems use the existing escalation and attention rendering.

## Feedback UX

Every material update (a `changed` ledger record; baselines and quiet
`every_run` receipts are excluded by `isMaterialUpdate`), on the Latest card and
each Updates row, offers:

1. **Useful** / **Not relevant**: `aria-pressed` chips POSTing
   `/monitors/{task_id}/updates/{update_id}/feedback` `{verdict}`. Optimistic,
   settled from the response (an idempotent `recorded:false` replay still
   carries the verdict and `feedback_id`), rolled back into the error banner on
   failure. The opposite verdict replaces the stored one. Disabled per update
   while in flight.
2. **Edit monitor**: the composer in edit mode.
3. **Pause monitor**: shown only while active and scheduled.

Stored verdicts load with the detail (`GET /monitors/{task_id}/feedback`,
merged by `feedbackState.ts`: one verdict per `update_id`, newest
`recorded_at` wins) as a "Marked useful/not relevant" badge. A failed read
shows no stored verdicts. Feedback is evidence only; it never rewrites the
contract client-side. The wire accepts a `note ≤500` that has no web input.

## Convert a task to a monitor

Eligible task rows carry **Convert to monitor** in the `⋯` menu
(`TaskCardMenu`, `convert_monitor`). `canConvertTaskToMonitor`
(`src/lib/monitors/convert.ts`) needs a persistent lifecycle, a real mutable
task (`source === 'task'`, not read-only), and not already a monitor: V3 rows
carry the server-owned `monitor_revision` (`> 0` = monitor; omitted while 0),
mapped to `Task.monitorRevision`. The server re-checks on POST.

It opens `MonitorComposer` with `mode="convert"` in a Modal (`TasksWorkspace`),
prefilled by `convertFormFromTask` (objective from description, else title;
title from title). The cadence editor becomes a read-only "Keeps its current
schedule: …" (`keptScheduleSummary`), because the body is `{spec, title?}`
only: the task keeps its id, schedule, history, executions and outputs. The
same review step applies; **Convert to monitor** POSTs
`/monitors/{task_id}/convert` (`ConvertMonitor{Request,Response}V1`). Refusals
render via `specReasonLabel` (`monitor_already_exists`,
`task_not_eligible_for_monitor`, `task_not_found`, standard `monitor_*`).
Conversion is user-explicit only, with no title heuristics. Limit:
`keptScheduleSummary` renders only cron schedules (the store's decoder drops
Interval/Once/OnEvent), so other schedules display "unscheduled" while the
backend keeps them verbatim.

## Tests

`src/lib/types/monitorContract.test.ts` (cross-platform fixtures in
`magician/tests/fixtures/monitors/`), `src/lib/monitors/{convert,pagination,specForm,feedbackState}.test.ts`,
`src/lib/magician/tasks/taskRoutes.test.ts`. Run:
`npx vitest run src/lib/monitors src/lib/magician/tasks/taskRoutes.test.ts`.

Mobile web stays behind the mobile blocker by design; iOS is
[magios/monitors](../magios/monitors.md).
