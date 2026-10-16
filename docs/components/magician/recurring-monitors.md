# Recurring Monitors (backend)

A Monitor is an existing persistent Task + `Task.schedule` + an optional typed
`monitor_spec` on `TaskManifest`: never a second scheduler, store, or runtime.

Web/Tauri: [unified-ui/monitors](../unified-ui/monitors.md) · iOS:
[magios/monitors](../magios/monitors.md) · user guide:
[Monitors](../../features/monitors.md) · chat tools: [chat-mode.md → Tool
Model](chat-mode.md#tool-model). Plan:
productization
· [Phase 0
baseline](../../archive/plans/2026-07-22-recurring-monitors-phase0-baseline.md).

## Model

Contract modules: `magician/src/magician_v2/monitors/` (`monitor_spec.rs`,
`monitor_run.rs`, `monitor_updates.rs`, `monitor_feedback.rs`, `provider.rs`;
see [Provider seam](#provider-seam-plan-32)), imported as
`magician_v2::monitors::` (as in `magician/tests/phase0_wire_oracles.rs`). HTTP:
`magician-api/src/monitors_api.rs` (registered in `magician-bin/src/main.rs`)
wraps `ArtifactV2Service` task paths. `DEFAULT_MONITOR_AGENT_ID` is
`personal-assistant` (`magician_v2::monitor_support`).

## Spec and admission

`monitor_spec.rs` defines `MonitorSpecV1` (`MonitorSources`, `MonitorMatchMode`,
`MonitorNotificationPolicy`). `TaskManifest` carries
`monitor_spec: Option<MonitorSpecV1>` (the sole discriminator, `None` on
ordinary tasks) and server-owned `monitor_revision: u32` (0 = not a monitor, 1
on the first spec write by `update_task`, +1 per spec edit, omitted from the
wire while 0).

`validate_and_normalize` gates every spec write: schema version 1
(`monitor_schema_version_unsupported`), bounded objective and string lists
(trimmed, deduped), http/https URLs parsed with `url::Url`, and at least one
surviving url/domain/query seed (`monitor_sources_required`). Failures are
stable snake_case reasons in 400 bodies.

## HTTP API

Under `/api/magician/v3`; monitor-scoped routes 404 (`monitor_not_found`) for
plain tasks. `TaskListItemV3` mirrors `monitor_revision` (`skip_serializing_if`
zero): `> 0` is a monitor, `0`/absent is convertible, with no per-row manifest
read.

| Route | Behavior |
|---|---|
| `POST /monitors` | Validate spec, `create_task` (Persistent, approved, title defaults to objective) + `update_task` spec via `magician_v2::monitor_support::create_monitor_task`; `201 {task_id, monitor_revision: 1}` |
| `GET /monitors?limit=&cursor=&state=` | Paged list (below) |
| `GET /monitors/{task_id}` | Spec, revision, schedule, state, reserved `system:monitor` tag (read-time only, never persisted) |
| `PATCH /monitors/{task_id}` | Title/spec/schedule; spec edits bump revision |
| `DELETE /monitors/{task_id}`, `POST /monitors/{task_id}/run` | Same paths as `delete_task_v3_handler` / `execute_task_v3_handler` |
| `POST /monitors/{task_id}/pause` · `/resume` | Flip `TaskSchedule.paused` via `update_task`; 409 `monitor_unscheduled` without a schedule |
| `POST /monitors/{task_id}/convert` | [Convert](#post-monitorstask_idconvert--explicit-taskmonitor-conversion) |
| `GET /monitors/{task_id}/runs?limit=` | Newest-first accepted runs, `{items, next_cursor: null, limit}` (1..=200); acceptance itself has no route |
| `GET /monitors/{task_id}/updates?limit=`, `GET /monitor-updates?limit=` | Newest first, same envelope (second scope-wide) |
| Feedback, `GET /monitors-metrics` | [Feedback, observability, metrics, retention](#feedback-observability-metrics-retention) |

`GET /monitors` returns `{items, next_cursor, limit, total, offset}` (limit
clamped 1..=200, default 50). `total` counts after the `state` filter, ignoring
paging; `offset` is where the cursor resolved (no `offset` param); a stale
cursor yields `offset == total` and an empty page, not an error. The cursor is
the last `task_id`, emitted with a `cur_` prefix, accepted without.
`state=active|paused` filters on `TaskSchedule.paused`. Rows are `TaskState`
projections (`cadence_summary`, `last_run_status`) without `next_run_at`, which
is in-memory scheduler state.

## Wire fixtures

`magician/tests/fixtures/monitors/*.json` (`MonitorSpecV1`, `MonitorRunResultV1`
changed/unchanged/degraded, list-page envelope, update detail), pinned by
`magician/tests/monitor_contract_fixtures.rs`, are shared by the web and iOS
contract tests, so all three clients share one wire. Pinned: `changed` carries
`change_fingerprint` and `unchanged` must not; `degraded` has
`complete_scan:false` and `possibly_removed:0`; `dedupe_key` contains task id,
revision, change fingerprint and channel. `max_runs` is enforced at boot
hydration, pre-fire, and post-fire.

## Run results and change ledger

`monitor_run.rs` defines `MonitorRunResultV1` (`MonitorFindingV1`,
`MonitorSourceOutcomeV1`, `MonitorCountsV1`, `MonitorEvidenceV1`,
`MonitorAccessProblemV1`, status/classification/source-outcome enums).
`validate_monitor_run_result(result, task_id, revision)` rejects, with stable
snake_case reasons:

- identity/revision mismatch; non-RFC3339 or inverted timestamps;
- counts: `scanned == new+updated+unchanged` on complete scans;
- status: `changed` ⇒ fingerprint + material finding; `unchanged` ⇒ neither;
  `degraded` ⇒ `complete_scan:false`; `baseline` ⇒ no removals or fingerprint
  (`monitor_run_baseline_forbids_removals`,
  `monitor_run_baseline_forbids_change_fingerprint`); `failed` ⇒ no fingerprint
  (`monitor_run_failed_forbids_change_fingerprint`);
- `complete_scan` inconsistent with source outcomes;
- **removal safety**: `possibly_removed` is 0 unless the scan was complete;
- bounds: `MONITOR_RUN_MAX_FINDINGS` = 200,
  `MONITOR_RUN_MAX_EVIDENCE_PER_FINDING` = 20.

### Stable identity and fingerprints

No LLM or I/O. `stable_key` is `native:<id>`, else `url:<host><path>` (canonical
URL with `utm_*`, `fbclid`, `gclid` and fragments stripped, host lowercased,
trailing slash dropped, query sorted), else
`composite:<source>|<title>|<sorted entities>` (lowercased, whitespace
collapsed). Fingerprints are order-independent 16-hex blake3:
`content_fingerprint` (`cf_`) over fact fields (title, entities, published_at,
canonical URL; not prose/evidence); `run_fingerprint` (`rf_`) over sorted
`(stable_key, content_fingerprint, classification)` triples + monitor identity;
`change_fingerprint` (`chg_`) over material triples only, absent when none.

`compare_runs(previous_cursor, incoming)` finalizes server-side, overwriting the
model's claims: new key ⇒ `new`, changed content ⇒ `updated`, else `unchanged`.
The first accepted run is `baseline`, never material, and never carries
`change_fingerprint` on the wire (`finalize_run_result`; an opted-in
`notify_initial_baseline` dedupes on execution id). `counts.unchanged` is the
listed count, never `scanned - new - updated`; a scan claiming completeness
without listing every item persists `complete_scan: false`.

Removal (`MONITOR_REMOVAL_MISS_THRESHOLD` = 2): a missing key accrues a miss
only if this and the previous accepted scan were complete and fully enumerated
(`counts.scanned <= findings listed`); auth-failure/timeout/rate-limit/partial
scans never count. The second miss emits a material `possibly_removed` finding
(source `change_ledger`) and drops the key; a reappearance is `new`.

### Hot state, cold artifacts, acceptance

Hot: `TaskState.monitor_cursor: Option<MonitorCursorV1>` (serde-default, absent
on non-monitors): `last_accepted_execution_id` (idempotency key),
`last_accepted_change_fingerprint` (carried across quiet runs),
`last_complete_scan`, `recent_stable_keys`
(`{key, content_fingerprint, misses}`, cap `MONITOR_RECENT_STABLE_KEYS_CAP` =
500, seen-first order, least-recently-observed evicted), `source_failures` (cap
`MONITOR_SOURCE_FAILURES_CAP` = 50), `updated_at`.

Cold:
`ArtifactV2Service::accept_monitor_run(scope, task_id, execution_id, result)`
validates, runs `compare_runs`, and writes `monitor_run_result` to
`executions/{id}/artifacts/persisted_artifacts.json`, then the cursor, under the
task write guard. Re-acceptance returns the persisted outcome; `failed` runs are
kept but never advance the ledger. `AcceptedMonitorRun` carries `material` +
`would_notify` (`every_run`/`never`/`material_changes` + baseline opt-in).
`get_monitor_runs(scope, task_id, limit)` walks executions newest first.
Acceptance is single-pass and warn-and-continue; acceptance, projection,
feedback and metrics make no LLM calls. Runs are ordinary root executions
(budget: `max_iterations`, token caps).

## Execution context

Only when `manifest.monitor_spec` is `Some`, dispatch and resume in
`artifact_v2/service.rs` append `MONITOR_CONTEXT_V1` (marker-idempotent);
generic executions are byte-identical. The template lives only in the prompt
registry (`monitor_execution_context_v1` v1.0.0,
`data/magician_v2/prompts/monitor_execution_context_v1_v1.0.0.json`) and
requires the run to emit a `monitor_run_result` artifact matching
`MonitorRunResultV1`. Rust only formats variables
(`monitor_run::monitor_context_variables`); a missing template skips injection
with a loud warn (the run then cannot validate), never a compiled fallback.

## Terminal acceptance, notifications, Today, Attention

`ArtifactV2Service::persist_execution_outcome` calls
`finalize_monitor_run_at_terminal` on all three terminal paths (first Step-1
landing, both replay short-circuits), gated on `manifest.monitor_spec.is_some()`
+ `outcome.is_terminal` + `parent_execution_id.is_none()` (one boolean check for
non-monitors). `extract_monitor_run_result_artifact` takes the newest
`artifact_type == "monitor_run_result"` record (fallback: a `tool_output_file`
named `monitor_run_result*.json`) and calls `accept_monitor_run`. A
missing/invalid artifact or a failed/cancelled run gets a FAILED marker
(`accept_failed_monitor_run_marker`, reason in one `monitor_runtime` source
outcome); the ledger never advances and generic finalization is never blocked.

### Notification policy and durable dedupe

`project_monitor_run_outcome` runs after every acceptance (backend gating, never
prompt convention):

1. `monitor_updates::build_monitor_update` mints a `MonitorUpdateDetailV1`
   (`monitor_update_detail_v1.json`) for material runs (even under `never`),
   baselines, and `every_run` runs; `notification.emitted` = `would_notify`.
2. Dedupe key `principal/workspace:task_id:revision:change_fingerprint:channel`
   (v1 channel `today_changed`; execution id when no fingerprint);
   `update_id = mu_<16-hex blake3(dedupe key)>`.
3. Append to `tasks/{id}/state/monitor_updates.jsonl` under the write guard if
   the `update_id` is new.
4. If `AttentionFunnelStore` is wired (`set_attention_funnel_store`,
   `magician-bin/src/main.rs`), insert
   `event_id = monitor-notify:<blake3(dedupe key)>` (`UNIQUE event_id`,
   `INSERT OR IGNORE`); policy-suppressed updates log one idempotent
   `monitor-notify-suppressed:*` trace instead.

### Today Changed

`MonitorActionAdapter`, the third `TodayActionAdapter`
(`feed/action_adapter.rs`, kind `monitor_update`), is navigation-only:
`open_task` to `/tasks?type=monitors&selected={task_id}[&update={update_id}]`
via `monitor_deep_link` (also card `source_url` and access escalations);
`plan_action` refuses. `FeedApi::today` merges `today_monitor_update_items` into
Changed, carding only `update_projects_to_changed` records (emitted and
`changed`/`baseline`); metadata: `monitor_task_id`, `update_id`,
`change_fingerprint`, `execution_id`; title is the update headline. Unchanged,
degraded and quiet `every_run` runs never reach Today. The item id embeds
`update_id`, so a dismissal suppresses exactly one fingerprint. No Followups
projection.

### Attention: access problems

`RouteReason::AccessProblemDetected` +
`AttentionRouteContext.access_problem_detected` → `NeedsYou`
(`attention_funnel.rs`). `advance_source_failures`: ok+complete resets, failing
increments, unscanned carries, failed markers do nothing. At
`MONITOR_SOURCE_FAILURE_ATTENTION_THRESHOLD` = 2 an `access_problem` creates
`monitor_access_problem:<task>:<blake3(source)>` (item_type `escalation` +
`needs_action` → `escalations`, Today `Needs You`). An ok+complete read removes
it, records `feed_attention_dismissals` and traces
`monitor_source_access_restored`. A new streak clears the stale dismissal; an
owner's dismissal of an ongoing streak stands.

## Chat tools

`magician/src/magician_v2/execution/embedded_pack_defs/{preview_monitor,create_monitor,update_monitor}.yaml`,
granted to the personal-assistant template beside `create_task`. Deterministic
(`validate_and_normalize` + `TaskSchedule` parser, no nested LLM);
`MONITOR_CONTEXT_V1` stays the only monitor prompt.

- **`preview_monitor`** (`compiled_handlers/preview_monitor.rs`, core
  `compiled_handlers/monitor_tools.rs`): store-free; returns spec, schedule,
  `cadence_summary` and `preview_fingerprint` (`mpv_<16-hex blake3>` of
  normalized spec + schedule, `monitor_spec::monitor_contract_fingerprint`).
- **`create_monitor`**: re-validates and needs a matching preview
  (`monitor_preview_required` / `monitor_preview_stale`), then
  `monitor_support::create_monitor_task` (revision 1, `personal-assistant`).
- **`update_monitor`**: mirrors `PATCH /monitors/{task_id}` (`monitor_not_found`
  for plain tasks; given fields replace, `[]` clears; merged spec re-validated;
  only spec edits bump revision).

## POST /monitors/{task_id}/convert — explicit task→monitor conversion

```text
POST /api/magician/v3/monitors/{task_id}/convert
  body: {"spec": MonitorSpecV1, "title"?: string}
  200: {"task_id", "monitor_revision": 1, "converted": true}
  404 task_not_found (addresses a TASK, not a monitor)
  409 monitor_already_exists (has a spec, incl. a soft-archived monitor)
  409 task_not_eligible_for_monitor (Internal-lifecycle or archived;
      no schedule is fine: run-on-demand)
  400 the standard monitor_* admission reasons
```

The task keeps id, schedule, history and outputs; the spec goes through
`validate_and_normalize` and the `update_task` spec arm (revision 1, optional
retitle). User-explicit only (web Tasks-row menu, iOS task-actions sheet, API;
no heuristic path):
[web](../unified-ui/monitors.md#convert-a-task-to-a-monitor) ·
[iOS](../magios/monitors.md#convert-a-task-to-a-monitor). Emits
`monitor_created` with `converted: true` (`converted_monitor_event_payload`); a
retry is 409.

### Scout / `vc-researcher` identity audit (plan Phase 7 checklist 2)

The VC Worker Scout (`vc-researcher`) is unrelated: monitor tools go only to
`personal-assistant`, nothing spawns `vc-researcher`, and the noun is Monitor.

## Feedback, observability, metrics, retention

### Feedback

```text
POST /api/magician/v3/monitors/{task_id}/updates/{update_id}/feedback
  body: {"verdict": "useful" | "not_relevant", "note": optional string ≤500}
  200: {"task_id","update_id","verdict","recorded":true|false,"feedback_id":"mf_<hash>"}
  404 monitor_not_found / update_not_found · 400 monitor_feedback_verdict_invalid
GET  /api/magician/v3/monitors/{task_id}/feedback?limit=
  {items:[{feedback_id,update_id,verdict,note?,recorded_at}], next_cursor:null, limit}
```

`feedback_id = mf_<16-hex blake3("principal/workspace:task:update:verdict")>`
(`monitor_feedback.rs`). Re-posting the current verdict replays
(`recorded: false`, same id, no write); another verdict appends; reads fold to
the latest per update, and flipping back re-derives the original id. Ledger:
`tasks/{id}/state/monitor_feedback.jsonl` beside `monitor_updates.jsonl`, via
`ArtifactV2Service::record_monitor_update_feedback` under the task write guard.
`not_relevant` copies the finding `stable_keys` + `content_fingerprints` and the
update's `execution_id` / `change_fingerprint` into an `evidence` block (not on
the GET wire). Feedback never rewrites the contract in v1; notes over 500 chars
are truncated.

### Observability events

All 13 events are `AttentionRouteEvent` rows (SQLite, UNIQUE `event_id` +
`INSERT OR IGNORE`, deterministic ids, so replays emit nothing). Traces use
`ArtifactV2Service::append_monitor_trace_event`
(`monitor-obs:<trace>:<blake3(seed)>`, metadata
`producer: monitor_observability` + `trace` + `monitor_task_id`).

| Event | Seam | Dedupe seed |
|---|---|---|
| `monitor_created` | `monitor_support::create_monitor_task` (HTTP + chat tool) | task id |
| `monitor_updated` | `record_monitor_updated_event` (PATCH + `update_monitor`) | task id + revision + `updated_at` |
| `monitor_paused` / `monitor_resumed` | `set_monitor_schedule_paused` | task id + state + `updated_at` |
| `monitor_run_started` | `project_monitor_run_outcome` (at acceptance; `started_at` from result) | execution id |
| `monitor_run_completed` | same, every accepted run incl. failed markers | execution id |
| `monitor_run_degraded` | same, status `degraded` | execution id |
| `monitor_change_detected` | same, material runs | execution id |
| `monitor_notification_sent` | `record_monitor_notification_outcome` | §7.4 dedupe key |
| `monitor_notification_suppressed` | same, no Changed notification | dedupe key, or execution id for quiet runs with no record |
| `monitor_source_access_failed` | `project_monitor_access_attention` | item id + streak length |
| `monitor_source_access_restored` | `resolve_monitor_access_problem` | item id + execution id |
| `monitor_feedback_recorded` | `record_monitor_update_feedback` (new verdicts) | feedback id + recorded_at |

`monitor_run_started` fires at acceptance because `goal_with_monitor_context`
has no store access. `MonitorNotificationSuppressionReason`
(`monitor_updates.rs`, via `notification_suppression_reason`): `policy_never`,
`unchanged` (nothing material under `material_changes`), `dedupe_replay` (§7.4
dedupe row existed), `quiet_every_run` (Updates only, no Changed card).

### Aggregate metrics

`GET /api/magician/v3/monitors-metrics`:

```text
{ monitors, counts: {active, paused, degraded_last_run, failed_last_run},
  material_change_rate, suppression_rate, source_failure_rate,
  window: {runs_per_monitor: 50, runs_considered} }
```

Computed per request (`monitors_api::aggregate_monitor_metrics`) over the newest
`MONITOR_METRICS_RUN_WINDOW` = 50 runs per monitor from `get_monitor_runs`.
Rates: `material_change_rate` (share `changed`), `suppression_rate` (no
notification, `would_notify` under the current policy; past policy changes are
not replayed), `source_failure_rate` (any non-ok or incomplete source, access
problem, or failed marker). Per-run detail: Runs (`source_outcomes`, counts,
`access_problem`), Updates (`notification`), funnel event metadata (suppression
reason); latency and LLM cost stay in execution telemetry.

### Bounded retention

`monitor_updates.jsonl` and `monitor_feedback.jsonl` cap at
`MONITOR_LEDGER_RETENTION_CAP` = 500; appends below it are O(1), past it an
atomic rewrite (`write_jsonl_records_atomic_path`, from
`ArtifactV2Service::append_monitor_ledger_bounded`) keeps the newest 500.
Deterministic `update_id` / `feedback_id` keep dedupe for surviving records; an
evicted `change_fingerprint` that re-emits is still caught by the SQLite funnel
row as `monitor_notification_suppressed(dedupe_replay)`. Run-result artifacts
are uncapped (execution retention).

## Evals

- `make eval-monitor-golden` → `scripts/eval-monitor-change-ledger.py`
  (`cargo test -p magician --test monitor_golden_scenarios`, report in
  `coverage/evals/monitor/latest.{html,json}`): fixtures in
  `magician/tests/fixtures/monitors/golden/` drive `validate` → `compare_runs` →
  `finalize` → `would_notify` → `build_monitor_update` /
  `update_projects_to_changed`, no LLM or server.
- `make eval-monitor-live` → `scripts/eval-monitor-live.py`: serves local
  fixture pages and drives scratch monitors in the isolated
  `live-eval/monitoring` scope over the v3 API (`MAGICIAN_BASE_URL`, default
  `http://127.0.0.1:3002`) over baseline, changed, unchanged+quiet-run and
  degraded+401; not the Today Changed card.

## Provider seam (plan 3.2)

Per [platform layering & app
extraction](../../archive/plans/2026-08-26-platform-layering-and-app-extraction-plan.md)
3.2, product logic is a seam module in `magician/src/magician_v2/monitors/` (not
an app package; wire shapes unchanged). `monitors/provider.rs`
`decide_run_acceptance(spec, task_id, revision, execution_id, previous cursor, incoming, now)`
returns the `MonitorRunAcceptanceDecision` (failed-run carve-out, comparison,
next cursor, finalized result, notify projection); `accept_monitor_run` in
`artifact_v2/service.rs` only does store integration (write guard, task read,
artifact, cursor) and never re-derives policy. Creation stays on `create_task` +
`update_task`; fetching stays on the `content_read`/`content_search` ladder (the
seam never fetches). `magician_v2::monitor_support` stays put: its schedule
helpers serve non-monitor callers (`storage/list_index`).

## Known limitations

- **`list_monitors` is a full-scope task scan.** `GET /monitors` walks
  `list_tasks` with one `get_task` record read per scope task (the listing
  projection lacks the manifest discriminator). Build an index past ~500 tasks
  or on `/monitors` latency.
- **Soft delete keeps the update ledger, intentionally.** Default
  `DELETE /monitors/{id}` archives but keeps
  `tasks/{id}/state/monitor_updates.jsonl` (restores keep dedupe history);
  `?remove_files=true` removes it, so a recreated monitor re-baselines.
- **Cursor pagination under concurrent mutation.** The cursor resolves by
  position in a re-sorted (`updated_at` desc) snapshot, so mutating it mid-walk
  ends paging early. Fix: an opaque `(updated_at, task_id)` cursor.
- **Scope-wide update and metrics reads.** `GET /monitor-updates` walks the
  scope and reads each whole ledger (≤500) before sorting: O(monitors × ledger);
  `GET /monitors/{id}/runs` reads one artifact index per execution until
  `limit`; `GET /monitors-metrics` adds up to 50 per monitor (diagnostics only).
