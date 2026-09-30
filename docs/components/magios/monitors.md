# Recurring Monitors (iOS / Magios)

Backend contract: [recurring-monitors](../magician/recurring-monitors.md) ·
web parity reference: [unified-ui/monitors](../unified-ui/monitors.md) ·
plan: Recurring Monitors productization
(§9.3).

A Monitor on iOS is the same canonical scheduled task every other client
manages — the app talks only to the `/api/magician/v3/monitors` routes
(including `convert`) and renders the wire shapes pinned by the canonical
fixtures in `magician/tests/fixtures/monitors/`.

## Surface map

- **Monitors lane** — a third segment on the native Tasks surface
  (`TaskLane.monitors`), rendered by `MonitorsLaneView`: cursor-paginated rows
  (state badge, health badge when not `ok`, the server's `cadence_summary`
  verbatim, last-run status + relative time), `All | Active | Paused` chips on
  the server-side `state` query, placeholder rows on first load, empty state
  with inline Create, error banner + Retry (also the offline state), auto-page
  plus a Load-more footer, and pull-to-refresh.
- **Detail** — `MonitorDetailView`, mirroring web `MonitorDetailPanel`:
  **Latest** (newest update record), **Updates** (durable notification ledger;
  un-emitted records marked "Not notified"; a deep-linked `update_id` is
  highlighted and scrolled to), **Runs** (every accepted `MonitorRunResultV1`
  incl. unchanged/degraded, with per-source outcomes and the access problem),
  **Settings** (typed contract, revision, cadence, fire count). Actions: Run now
  / Pause / Resume / Edit / Delete (confirmed; soft archive). Execution-level
  inspection stays on the shared task detail (`DeepWorkPanel`, via "Open task
  view").
- **Create/edit sheet** — `MonitorComposerView` + `MonitorComposerViewModel`:
  simple fields (title, objective, URLs, cadence preset/custom cron/on-demand,
  notification policy), with domains / search phrases / include-exclude rules /
  match mode / signed-in sources / baseline-notify behind "More options".
  Submitting enters a **review-before-activate** step showing the normalized
  contract and exact `cadence_summary`; the POST/PATCH happens only from that
  step, with a double-submit lock.
  **Edit-mode cadence rule:** "On demand only" is hidden in edit mode, because
  cadence "none" makes the PATCH omit `schedule` (a silent no-op; the review
  step says "No cadence change"). It stays visible only while it is the current
  selection — a monitor with an Interval/Once/OnEvent (or no) schedule opens at
  "none", and saving does not clear that schedule.
- **Form logic** — `MonitorForm` mirrors the backend admission gate
  (`monitor_spec.rs::validate_and_normalize`) reason-for-reason
  (`monitor_objective_required`, `monitor_sources_required`,
  `monitor_source_url_scheme_unsupported`, …) so local rejection shows the same
  stable reasons a server 400 would; the server stays authoritative.
  `Monitors.cadenceSummary` mirrors `monitors_api::cadence_summary`
  (`Cron 0 6 * * 1 (America/Los_Angeles)` / `Every 3600s` / `unscheduled`).

## Feedback UX

Every material update card (a `changed` ledger record,
`Monitors.isMaterialUpdate`; baselines and quiet `every_run` receipts take no
verdict) offers, in both Latest and Updates (shared `updateCard`):

1. **Useful** / **Not relevant** — POST
   `/monitors/{task_id}/updates/{update_id}/feedback`. Optimistic; settles from
   the response (the idempotent `recorded:false` replay still carries the
   authoritative verdict + `feedback_id`); rolls back into the action alert on
   failure. The opposite verdict replaces the stored one. Chips disable
   per-update while that POST is in flight and never lock the lifecycle row.
2. **Edit monitor** — the composer edit sheet.
3. **Pause monitor** — shown only while active and scheduled.

Stored verdicts load with the detail (`GET /monitors/{task_id}/feedback` →
`Monitors.feedbackStates(from:)`: one verdict per `update_id`, newest
`recorded_at` wins regardless of order). The read is partial-tolerant: failure
degrades to no stored verdicts. Errors: 404 `update_not_found` → `.notFound`;
400 `monitor_feedback_verdict_invalid` → `.validation`. Feedback is evidence
only and never rewrites the contract client-side. The wire accepts `note ≤500`;
iOS sends verdicts only (web parity).

## Convert a task to a monitor (Phase 7)

Eligible cards on the plain Tasks lane offer **Convert to monitor** in
`TaskCardActionsSheet`. Eligibility is `TaskV3.canConvertToMonitor`: persistent
lifecycle and not already a monitor (`monitor_revision > 0` = monitor; omitted
while 0, decoded `?? 0`). The server re-checks.

The composer opens in `Mode.convert(taskID:keptCadence:)`, prefilled by
`MonitorForm.convertPrefill` (objective ← description, title fallback; cadence
pinned `none`). Cadence is replaced with a read-only "Keeps its current
schedule: …" line (`TaskV3.keptScheduleSummary`, cron only — other kinds show
"unscheduled" while the backend keeps them verbatim), because the body is
`{spec, title?}` only: the task keeps its id, schedule, history, executions and
outputs. After the review step, `POST /monitors/{task_id}/convert`; 409 maps to
`.conflict(reason:)` ("already a monitor" / "can't be converted"). On success
the view switches to the Monitors lane. Conversion is user-explicit only — no
title-text heuristics.

## Files

All monitor code is namespaced under `Monitors`, in `magios/Magios/Monitors/`:

| File | Contents |
|---|---|
| `MonitorModels.swift` | Typed wire: `SpecV1`, run-result family, `UpdateDetailV1`, pages, create/patch bodies, externally-tagged `ScheduleWire` (`{"kind":{"Cron":{…}}}`), cadence-summary mirror. Known variants decode strictly (`{"Cron":{}}` throws); unknown keys (`Once`/`OnEvent`/future) ride `.other`. |
| `MonitorAPIClient.swift` | `Monitors.APIClient` — injected baseURL/scope/transport, shared bearer auth, one call per route, stable-reason error mapping. |
| `MonitorForm.swift` | Admission-mirror validation, cadence presets, edit-form round trip, `convertPrefill`, labels. |
| `MonitorsViewModel.swift` | `MonitorsListViewModel` (cursor pagination + generation guard + dedupe by task_id; `refreshLoadedSpan()` re-reads held rows and re-primes the cursor — an unknown cursor resolves to end-of-list), `MonitorDetailViewModel`, `MonitorComposerViewModel`, `Monitors.liveClient()`. |
| `MonitorsListView.swift` / `MonitorDetailView.swift` / `MonitorComposerView.swift` | SwiftUI surfaces. |
| `MonitorDeepLink.swift` | `Monitors.DeepLinkTarget` + inbound resolvers. |

Integration points: `TasksViewModel.swift`, `TasksView.swift`,
`AppActions.swift` (`requestMonitor(taskID:updateID:)`), `AppTabView.swift`,
`App.swift` (`magican://monitor` host), `TodayView.swift`.

## Deep links

Three inbound shapes resolve to one target (`Monitors.DeepLinkTarget` →
`AppActions.requestMonitor` → Tasks → Monitors lane → detail with the update
highlighted):

1. **Today `monitor_update` cards** — `metadata.monitor_task_id` +
   `metadata.update_id` are authoritative; the canonical `source_url` route is
   the fallback for older records.
2. **The canonical route** `/tasks?type=monitors&selected=task_…&update=mu_…`
   (`Monitors.parseTasksRoute`).
3. **`magican://monitor/{task_id}?update=mu_…`** (`Monitors.parseDeepLinkURL`).

`magican://task/{id}` also falls back to monitor mode: the task-list projection
cannot carry `monitor_spec`, so `TasksView.revealRequestedTask` probes
`GET /monitors/{id}` — 200 opens the monitor, anything else proceeds as a plain
task. The probe uses the lane's injectable client, and every terminal outcome
consumes the pending target exactly once (`Monitors.finishTaskLink`), so an
offline/deleted target degrades once instead of re-probing on every publish.

APNs registration exists (build-gated by `MAGIOS_REMOTE_PUSH`), but
monitor-update notifications do not open a monitor; the intended landing path
is `magican://monitor/{task_id}?update={update_id}`.

## Scope

Web parity: same list fields and cursor envelope, state filter, four detail
sections, exact-update highlight, actions, simple/advanced composer with review
step, convert, admission-mirror reasons and cadence string. Not built on iOS:
monitor-update push opening, the feedback note field, and the
`/monitor-updates` feed as its own surface (Today Changed is the product
surface).

Tests (in `make test-ios`): `MonitorContractFixtureTests` (decodes every
canonical fixture, so a wire change fails Rust, vitest and iOS together),
`MonitorAPIClientTests`, `MonitorsViewModelTests`, `MonitorFormTests`,
`MonitorDeepLinkTests`.
