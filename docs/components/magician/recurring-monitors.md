# Recurring Monitors (backend)

A Monitor is an existing persistent Task + `Task.schedule` + an optional typed
`monitor_spec` on `TaskManifest` — never a second scheduler, store, or runtime.

Web/Tauri: [unified-ui/monitors](../unified-ui/monitors.md) · iOS:
[magios/monitors](../magios/monitors.md) · user guide:
[Monitors](../../features/monitors.md) · chat tools:
[chat-mode.md → Tool Model](chat-mode.md#tool-model). Plan:
productization
· Phase 0 baseline.

## Model

Contract modules live in `magician/src/magician_v2/monitors/`
(`monitor_spec.rs`, `monitor_run.rs`, `monitor_updates.rs`,
`monitor_feedback.rs`, `provider.rs`) — see
[Provider seam](#provider-seam-plan-32). Imports (including
`magician/tests/phase0_wire_oracles.rs`) use `magician_v2::monitors::`.
HTTP handlers: `magician-api/src/monitors_api.rs`, registered from
`magician-bin/src/main.rs`. Thin wrappers over the same
`ArtifactV2Service` paths the task endpoints use.
`DEFAULT_MONITOR_AGENT_ID` is `personal-assistant`
(`magician_v2::monitor_support`).

## Spec and admission

`monitor_spec.rs` defines `MonitorSpecV1` (`MonitorSources`,
`MonitorMatchMode`, `MonitorNotificationPolicy`). `TaskManifest` carries:

- `monitor_spec: Option<MonitorSpecV1>` — the sole monitor discriminator
  (`None` on every ordinary task)
- `monitor_revision: u32` — server-owned: 0 = not a monitor, set to 1 on
  the first spec write by `update_task`, +1 on every spec edit; omitted
  from the wire while 0 so old manifests stay byte-identical

`validate_and_normalize` is the single gate for every spec write: schema
version must be 1 (`monitor_schema_version_unsupported` otherwise);
objective trimmed/bounded; string lists trimmed/deduped/bounded; source
URLs parsed with `url::Url` and restricted to http/https; at least one of
urls/domains/query_seeds must survive (`monitor_sources_required`).
Failures are stable snake_case reasons rendered as 400 bodies.

## HTTP API

Scoped under `/api/magician/v3`. Every monitor-scoped route 404s
(`monitor_not_found`) for plain tasks. `TaskListItemV3` mirrors
`monitor_revision` (`skip_serializing_if` zero), so clients derive
"already a monitor" (`> 0`) vs "eligible for convert" (`0`/absent) from
the list row with no per-row manifest read.

- `POST /monitors` — validate spec → `create_task` (Persistent, approved,
  title derived from the objective when absent) → `update_task` attaches
  the spec via `magician_v2::monitor_support::create_monitor_task`;
  returns `201 {task_id, monitor_revision: 1}`.
- `GET /monitors?limit=&cursor=&state=` — `{items, next_cursor, limit,
  total, offset}` (limit clamped 1..=200, default 50). `total` is the
  corpus AFTER the `state` filter and is never reduced by paging; `offset`
  is the position the cursor resolved to (no `offset` parameter). A stale
  cursor reports `offset == total` and an empty last page rather than an
  error. Cursor = previous page's last `task_id`, emitted with the `cur_`
  prefix and accepted without it. `state=active|paused` filters on
  `TaskSchedule.paused`. Rows are cheap `TaskState` projections
  (`cadence_summary`, `last_run_status`). `next_run_at` is not on the
  list row (next-fire is in-memory scheduler state).
- `GET /monitors/{task_id}` — spec, revision, schedule, state, and the
  reserved `system:monitor` tag projected at read time only (never
  persisted).
- `PATCH /monitors/{task_id}` — title/spec/schedule; a spec edit bumps
  the server-owned revision.
- `DELETE /monitors/{task_id}` and `POST /monitors/{task_id}/run` — the
  same delete/execute paths `delete_task_v3_handler` /
  `execute_task_v3_handler` use.
- `POST /monitors/{task_id}/pause` · `/resume` — flip
  `TaskSchedule.paused` through `update_task` (409 `monitor_unscheduled`
  when the monitor has no schedule).
- `POST /monitors/{task_id}/convert` — see
  [Convert](#post-monitorstask_idconvert--explicit-taskmonitor-conversion).
- `GET /monitors/{task_id}/runs?limit=` — newest-first accepted runs in
  `{items, next_cursor: null, limit}` (limit 1..=200). No accept HTTP
  route; acceptance is a service call from the execution pipeline.
- `GET /monitors/{task_id}/updates?limit=` and scope-wide
  `GET /monitor-updates?limit=` — newest first, same simple envelope.
- Feedback and `GET /monitors-metrics` — see
  [Feedback, observability, metrics, retention](#feedback-observability-metrics-retention).

## Wire fixtures

Canonical fixtures in `magician/tests/fixtures/monitors/*.json`
(`MonitorSpecV1`, `MonitorRunResultV1` changed/unchanged/degraded, list-page
envelope, update detail) are pinned by
`magician/tests/monitor_contract_fixtures.rs` and read by the web and iOS
contract tests, so all three clients share one wire.

Pinned semantics: a `changed` run carries `change_fingerprint`, an
`unchanged` run must not; a `degraded` run has `complete_scan:false` and
`possibly_removed:0`; the notification `dedupe_key` contains monitor task
id, monitor revision, change fingerprint, and channel.

`max_runs` is enforced at boot hydration, pre-fire, and post-fire.

## Run results and change ledger

`monitor_run.rs` defines `MonitorRunResultV1` (`MonitorFindingV1`,
`MonitorSourceOutcomeV1`, `MonitorCountsV1`, `MonitorEvidenceV1`,
`MonitorAccessProblemV1` and the status/classification/source-outcome
enums).

`validate_monitor_run_result(result, task_id, revision)` rejects with
stable snake_case reasons: identity/revision mismatch, non-RFC3339 or
inverted timestamps, counts arithmetic (`scanned == new+updated+unchanged`
on complete scans only), status coherence (`changed` ⇒ fingerprint +
material finding; `unchanged` ⇒ neither; `degraded` ⇒
`complete_scan:false`; `baseline` ⇒ no `possibly_removed`
counts/classifications and no change fingerprint —
`monitor_run_baseline_forbids_removals` /
`monitor_run_baseline_forbids_change_fingerprint`; `failed` ⇒ no change
fingerprint — `monitor_run_failed_forbids_change_fingerprint`),
`complete_scan` vs source outcomes, **removal safety**
(`possibly_removed` counts and classifications must be 0 unless the scan
was complete), and bounds (`MONITOR_RUN_MAX_FINDINGS` = 200,
`MONITOR_RUN_MAX_EVIDENCE_PER_FINDING` = 20).

### Stable identity and fingerprints

Deterministic, no LLM, no I/O. `stable_key`: source-native id →
`native:<id>`; else normalized canonical URL → `url:<host><path>`
(tracking params `utm_*`/`fbclid`/`gclid` + fragments stripped, host
lowercased, trailing slash dropped, surviving query pairs sorted); else
`composite:<source>|<title>|<sorted entities>` (lowercased, whitespace
collapsed).

Fingerprints are blake3, 16-hex: `content_fingerprint` (`cf_`) over the
FACT fields (normalized title, sorted entities, published_at, normalized
canonical URL — prose/evidence excluded); `run_fingerprint` (`rf_`) over
all findings' sorted `(stable_key, content_fingerprint, classification)`
triples + monitor identity; `change_fingerprint` (`chg_`) over the sorted
MATERIAL triples, absent when nothing is material. All
ordering-independent.

`compare_runs(previous_cursor, incoming)` finalizes everything
server-side (the model's claimed classifications/fingerprints/status are
overwritten): new stable item ⇒ `new`; same key + different content
fingerprint ⇒ `updated`; same fingerprint ⇒ `unchanged`. First accepted
run ⇒ `baseline`, never material. Comparison computes a fingerprint over
a baseline's findings for cursor continuity, but the finalized baseline
never carries `change_fingerprint` on the wire (`finalize_run_result`; an
opted-in `notify_initial_baseline` notification dedupes on the execution
id instead). Finalized `counts.unchanged` is the actual number of listed
unchanged findings — never `scanned - new - updated` — and a scan that
claimed completeness without enumerating every scanned item persists
`complete_scan: false`.

Two-scan removal (`MONITOR_REMOVAL_MISS_THRESHOLD` = 2): a ledger key
absent from a scan accrues a miss only when this scan and the previous
accepted scan were both complete AND fully enumerated
(`counts.scanned <= findings listed`). Auth-failure/timeout/rate-limit/
partial scans never advance misses. The second consecutive qualifying
absence synthesizes a material `possibly_removed` finding (source
`change_ledger`) and drops the key (a later reappearance is `new`).

### Hot state, cold artifacts, acceptance

Hot — `TaskState.monitor_cursor: Option<MonitorCursorV1>` (serde-default;
non-monitors never carry it): `last_accepted_execution_id` (idempotency
key), `last_accepted_change_fingerprint` (carried forward across quiet
runs), `last_complete_scan`, `recent_stable_keys`
(`{key, content_fingerprint, misses}`, capped at
`MONITOR_RECENT_STABLE_KEYS_CAP` = 500 — seen-first ordering,
least-recently-observed tail evicted), `source_failures` (capped at
`MONITOR_SOURCE_FAILURES_CAP` = 50), `updated_at`.

Cold — `ArtifactV2Service::accept_monitor_run(scope, task_id,
execution_id, result)` validates, runs `compare_runs`, and persists the
finalized result as the durable `monitor_run_result` record in the
existing per-execution artifact index
(`executions/{id}/artifacts/persisted_artifacts.json`). Write order is
artifact-then-cursor under the task write guard; re-acceptance of an
already-accepted execution id returns the persisted outcome without
rewriting. `failed` runs persist for history but never advance the
ledger. Returned `AcceptedMonitorRun` carries `material` + `would_notify`
(`every_run`/`never`/`material_changes` + the baseline opt-in).

`get_monitor_runs(scope, task_id, limit)` walks the executions index
(newest first) and reads each persisted-artifact index.

The accept path is single-pass and warn-and-continue. Per-run budget is
the execution engine's existing `max_iterations` and token caps. Monitor
runs are ordinary root executions; acceptance/projection/feedback/metrics
add no LLM calls of their own.

## Execution context

When (and only when) `manifest.monitor_spec` is `Some`, the execution
goal gets the `MONITOR_CONTEXT_V1` block appended on both the
initial-dispatch and resume seams in `artifact_v2/service.rs`
(marker-idempotent). Generic executions are byte-identical.

The template lives in the prompt registry only:
`monitor_execution_context_v1` v1.0.0
(`data/magician_v2/prompts/monitor_execution_context_v1_v1.0.0.json`) —
renders the spec and the required output contract: end the run by
emitting a `monitor_run_result` JSON artifact matching
`MonitorRunResultV1`. Rust only formats variables
(`monitor_run::monitor_context_variables`); if the template is missing
the injection is skipped with a loud warn (the run then cannot validate)
rather than falling back to a compiled prompt string.

## Terminal acceptance, notifications, Today, Attention

`ArtifactV2Service::persist_execution_outcome` calls
`finalize_monitor_run_at_terminal` on all three terminal paths (first
Step-1 landing and both replay short-circuits). Strictly gated:
`manifest.monitor_spec.is_some()` + `outcome.is_terminal` + root
execution (`parent_execution_id.is_none()`). Non-monitor executions pay
one boolean check.

Artifact extraction (`extract_monitor_run_result_artifact`): newest
persisted-index record with `artifact_type == "monitor_run_result"`, with
a tolerant fallback for `tool_output_file` named
`monitor_run_result*.json`. Completed run + valid artifact →
`accept_monitor_run`. Completed run with a missing/unparseable/invalid
artifact, and every failed/cancelled run → a synthesized FAILED run
marker (`accept_failed_monitor_run_marker`) so Runs history stays
complete, the change ledger never advances, and the reason lands in a
single `monitor_runtime` source outcome. Generic finalization is never
blocked.

### Notification policy and durable dedupe

`project_monitor_run_outcome` runs after every acceptance (backend-owned;
gating is never a prompt convention):

1. `monitor_updates::build_monitor_update` mints a
   `MonitorUpdateDetailV1` (fixture `monitor_update_detail_v1.json`).
   Update-worthy = material runs (even under `never`, for Updates
   history), every baseline, and every run under `every_run`.
   `notification.emitted` = the `would_notify` projection.
2. Dedupe key is exactly
   `principal/workspace:task_id:revision:change_fingerprint:channel`
   (channel v1 = `today_changed`). Runs without a change fingerprint
   (quiet `every_run` receipts, empty baselines) key on the execution
   id. `update_id = mu_<16-hex blake3(dedupe key)>`.
3. Durable ledger: `tasks/{id}/state/monitor_updates.jsonl` under the
   task write guard, only when the deterministic `update_id` is not
   already present.
4. Per-channel dedupe row: when `AttentionFunnelStore` is wired
   (`set_attention_funnel_store`, `magician-bin/src/main.rs`), an
   `event_id = monitor-notify:<blake3(dedupe key)>` route event is
   inserted (`UNIQUE event_id`, `INSERT OR IGNORE`). Policy-suppressed
   updates append one idempotent `monitor-notify-suppressed:*` trace
   event instead.

### Today Changed

`MonitorActionAdapter` is the third registered `TodayActionAdapter`
(`feed/action_adapter.rs`, source kind `monitor_update`): navigation-only
(`open_task` deep-linking to
`/tasks?type=monitors&selected={task_id}[&update={update_id}]` via
`monitor_deep_link`; Today card `source_url` and access-problem
escalation use the same helper), metadata-based task reconciliation,
`plan_action` refuses.

`FeedApi::today` merges `today_monitor_update_items` into Changed: only
records with `update_projects_to_changed` (emitted AND status
`changed`/`baseline`) become cards. Unchanged and degraded runs never
reach Today, including quiet `every_run` receipts (those stay in Updates
history only). Card title = update headline; metadata carries
`monitor_task_id` + `update_id` + `change_fingerprint` + `execution_id`.

Dismissal: the Today item id embeds the deterministic `update_id`, so
dismissing a card suppresses exactly that change fingerprint; the next
different fingerprint mints a new id. No Followups projection.

### Attention: access problems

Funnel vocabulary: `RouteReason::AccessProblemDetected` +
`AttentionRouteContext.access_problem_detected` → `NeedsYou`
(`attention_funnel.rs`).

`advance_source_failures`: ok+complete resets, failing increments,
unscanned carries, failed-run markers change nothing.

When an accepted run carries `access_problem` AND that source's streak
has reached `MONITOR_SOURCE_FAILURE_ATTENTION_THRESHOLD` = 2, the
candidate materializes as `monitor_access_problem:<task>:<blake3(source)>`
(item_type `escalation` + `needs_action` → attention `escalations` and
Today `Needs You`).

Every source an accepted run reads ok+complete removes its item by
deterministic id (no-op when absent) and records
`feed_attention_dismissals`; `monitor_source_access_restored` lands as an
idempotent funnel trace. A new failure streak re-crossing the threshold
clears the stale dismissal tombstone; an owner's dismissal of an ongoing
streak stands.

## Chat tools

Schemas + guides live in capability YAML:
`magician/src/magician_v2/execution/embedded_pack_defs/{preview_monitor,create_monitor,update_monitor}.yaml`,
registered like every compiled tool. Granted to the personal-assistant
template next to `create_task`, whose behavior is unchanged.

Deterministic tools; interpretation stays in the calling model. They
validate through the same `validate_and_normalize` gate and the existing
`TaskSchedule` parser. No nested LLM call. `MONITOR_CONTEXT_V1` remains
the only registered monitor prompt.

- **`preview_monitor`** (`compiled_handlers/preview_monitor.rs` → core in
  `compiled_handlers/monitor_tools.rs`): builds + normalizes the spec,
  never touches the task store, returns the interpreted contract (spec +
  exact schedule + the same `cadence_summary` list rows show) plus a
  `preview_fingerprint` (`mpv_<16-hex blake3>` over the normalized spec +
  schedule; `monitor_spec::monitor_contract_fingerprint`).
- **`create_monitor`**: re-validates, recomputes the fingerprint, and
  refuses without a matching preview (`monitor_preview_required` /
  `monitor_preview_stale`). On match it creates through
  `monitor_support::create_monitor_task` (revision 1, default
  `personal-assistant` owner).
- **`update_monitor`**: mirrors `PATCH /monitors/{task_id}` — plain tasks
  refused with `monitor_not_found`, provided spec fields replace (explicit
  `[]` clears), merged spec re-validated, spec edits bump revision,
  title/schedule-only edits don't.

## POST /monitors/{task_id}/convert — explicit task→monitor conversion

```text
POST /api/magician/v3/monitors/{task_id}/convert
  body: {"spec": MonitorSpecV1, "title"?: string}
  200: {"task_id", "monitor_revision": 1, "converted": true}
  404 task_not_found (this route addresses a TASK, not a monitor)
  409 monitor_already_exists (already carries a spec, including a
      soft-archived former monitor — archive keeps the manifest)
  409 task_not_eligible_for_monitor (Internal-lifecycle tasks; archived
      tasks. A missing schedule is allowed — the monitor is run-on-demand)
  400 the standard monitor_* admission reasons
```

The task keeps its id, schedule, fire history, executions, and outputs.
Conversion validates through `validate_and_normalize` and attaches the
spec through the same `update_task` spec arm — `monitor_revision` starts
at 1 — and optionally retitles. User-explicit only: web Tasks-row menu,
iOS task-actions sheet, or a direct API call. No title-text/heuristic
path. Client convert flows:
[web](../unified-ui/monitors.md#convert-a-task-to-a-monitor-phase-7) ·
[iOS](../magios/monitors.md#convert-a-task-to-a-monitor-phase-7).

The convert seam emits `monitor_created` with `converted: true`
(deterministic seed = task id; a second attempt is 409). Payload builder
is `converted_monitor_event_payload`.

### Scout / `vc-researcher` identity audit (plan Phase 7 checklist 2)

The VC Worker named Scout (`vc-researcher`) is unrelated. Monitor tools
are granted only to `personal-assistant`; no monitor path selects or
spawns `vc-researcher`. The product noun is Monitor on every surface.

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
(`monitor_feedback.rs`). Re-posting the current verdict replays the
persisted record (`recorded: false`, same id, no write); posting the
other verdict appends a new record — latest wins per update (append-only
history; reads fold to the current verdict, newest first). Flipping back
re-derives the original deterministic id.

Durable per-task ledger: `tasks/{id}/state/monitor_feedback.jsonl` beside
`monitor_updates.jsonl`, written under the task write guard by
`ArtifactV2Service::record_monitor_update_feedback`.

A `not_relevant` verdict copies the implicated finding `stable_keys` +
`content_fingerprints` (plus the update's `execution_id` /
`change_fingerprint`) from the update record at write time into the
feedback record's `evidence` block. Feedback never rewrites the monitor
contract in v1. Evidence is stored but not on the GET list wire. Notes
longer than 500 chars are truncated at admission.

### Observability events

All 13 named events ride the attention-funnel store's
`AttentionRouteEvent` rows (SQLite, UNIQUE `event_id` + `INSERT OR
IGNORE`). Event ids are deterministic, so idempotent replays emit
nothing new. Lifecycle/run/feedback traces share
`ArtifactV2Service::append_monitor_trace_event`
(`monitor-obs:<trace>:<blake3(seed)>`, metadata
`producer: monitor_observability` + `trace` + `monitor_task_id`).

| Event | Seam | Dedupe seed |
|---|---|---|
| `monitor_created` | `monitor_support::create_monitor_task` (HTTP + `create_monitor` chat tool) | task id |
| `monitor_updated` | `record_monitor_updated_event` — PATCH + `update_monitor` | task id + revision + manifest `updated_at` |
| `monitor_paused` / `monitor_resumed` | `set_monitor_schedule_paused` | task id + state + manifest `updated_at` |
| `monitor_run_started` | `project_monitor_run_outcome` (at acceptance, not dispatch; `started_at` from the run result) | execution id |
| `monitor_run_completed` | same (every accepted terminal run, incl. failed markers) | execution id |
| `monitor_run_degraded` | same, when finalized status is `degraded` | execution id |
| `monitor_change_detected` | same, when the run is material | execution id |
| `monitor_notification_sent` | `record_monitor_notification_outcome` — the `monitor-notify:` row | §7.4 dedupe key |
| `monitor_notification_suppressed` | same — every accepted run that did not surface a Changed notification | dedupe key (recorded updates) / execution id (quiet runs with no record) |
| `monitor_source_access_failed` | `project_monitor_access_attention` | item id + streak length |
| `monitor_source_access_restored` | `resolve_monitor_access_problem` | item id + execution id |
| `monitor_feedback_recorded` | `record_monitor_update_feedback`, only on newly recorded verdicts | feedback id + recorded_at |

`monitor_run_started` is at acceptance because `goal_with_monitor_context`
sits on the generic execution hot path with no store access.

`MonitorNotificationSuppressionReason` (`monitor_updates.rs`):
`policy_never`, `unchanged` (nothing material under `material_changes` —
quiet unchanged/degraded/failed runs and non-opted baselines),
`dedupe_replay` (a newly recorded update whose §7.4 durable dedupe row
already existed), `quiet_every_run` (an `every_run` receipt on a quiet
run: recorded + emitted into Updates history, never a Today Changed
card). Mapping is `notification_suppression_reason`.

### Aggregate metrics

`GET /api/magician/v3/monitors-metrics`:

```text
{ monitors, counts: {active, paused, degraded_last_run, failed_last_run},
  material_change_rate, suppression_rate, source_failure_rate,
  window: {runs_per_monitor: 50, runs_considered} }
```

Computed per request from existing stores (task records +
`get_monitor_runs`, window = newest `MONITOR_METRICS_RUN_WINDOW` = 50
accepted runs per monitor). `material_change_rate` = fraction of
considered runs with status `changed`; `suppression_rate` = fraction that
notified nothing (`would_notify` recomputed against the current spec
policy — historical policy changes are not replayed);
`source_failure_rate` = fraction with any failing source (non-ok/
incomplete outcome, access problem, or failed marker). Math is
`monitors_api::aggregate_monitor_metrics`. Run latency and LLM cost stay
in existing execution records + LLM telemetry.

Per-run diagnostics use existing reads: Runs serve finalized
`MonitorRunResultV1` (per-source `source_outcomes`, counts,
`access_problem`); Updates serve the `notification` block; the
suppression reason is on the funnel event metadata.

### Bounded retention

Both per-task ledgers (`monitor_updates.jsonl`, `monitor_feedback.jsonl`)
are capped at `MONITOR_LEDGER_RETENTION_CAP` = 500. Appends below the cap
stay O(1); an append past it rewrites atomically
(`write_jsonl_records_atomic_path`) keeping the newest 500 including the
new record (`ArtifactV2Service::append_monitor_ledger_bounded`).

Dedupe survives compaction for surviving records because `update_id` /
`feedback_id` are deterministic hashes. A `change_fingerprint` older than
the 500-record horizon can re-emit after compaction; the durable
per-channel funnel row (SQLite, not subject to this compaction) still
catches it as `monitor_notification_suppressed(dedupe_replay)`.

Run-result artifacts are not part of this cap. Accepted run records live
in the per-execution artifact index and follow ordinary execution
retention/archival.

## Evals

`make eval-monitor-golden` → `scripts/eval-monitor-change-ledger.py`, a
thin runner over `cargo test -p magician --test monitor_golden_scenarios`
that writes `coverage/evals/monitor/latest.{html,json}`. Fixtures in
`magician/tests/fixtures/monitors/golden/` drive
`validate` → `compare_runs` → `finalize` → `would_notify` →
`build_monitor_update` / `update_projects_to_changed`. No LLM, no server.

`make eval-monitor-live` → `scripts/eval-monitor-live.py`: serves local
fixture pages, creates scratch monitors in the isolated
`live-eval/monitoring` scope over the v3 API (`MAGICIAN_BASE_URL`, default
`http://127.0.0.1:3002`), and drives baseline / changed / unchanged+quiet-run
/ degraded+401. It does not cover the Today Changed card.

## Provider seam (plan 3.2)

Plan: platform layering & app extraction
workstream 3.2. Product logic is a seam-registered module in
`magician/src/magician_v2/monitors/` (default form; not an app package).
Wire shapes are unchanged.

`monitors/provider.rs`: `decide_run_acceptance(spec, task_id, revision,
execution_id, previous cursor, incoming, now)` is the one decision the
run-acceptance handler calls — failed-run carve-out, §7 comparison, next
hot-state cursor, finalized persisted result, and notify projection
(`MonitorRunAcceptanceDecision`). `accept_monitor_run` in
`artifact_v2/service.rs` is task-store integration: write guard, task
read, the durable `monitor_run_result` artifact, and cursor persistence.

Creation stays the ordinary V3 task path (`create_task` + `update_task`;
composition in `magician_v2::monitor_support::create_monitor_task`).
Content acquisition stays on the `content_read`/`content_search` ladder;
nothing in the seam fetches. The service never re-derives product policy.
`magician_v2::monitor_support` stays put: its schedule helpers are shared
with non-monitor callers (`storage/list_index`).

## Known limitations

- **`list_monitors` is a full-scope task scan.** `GET /monitors` walks
  `list_tasks` and issues one `get_task` record read per task in the
  scope on every request (the monitor discriminator lives on the
  manifest, which the listing projection does not carry) — O(N) in scope
  tasks, record reads only. Trigger to build a monitor index: scopes with
  more than ~500 tasks, or observed list latency on `/monitors`.
- **Soft delete leaves the update ledger on disk — intentionally.**
  Default `DELETE /monitors/{id}` archives the task but keeps
  `tasks/{id}/state/monitor_updates.jsonl`; a restored monitor keeps its
  dedupe history. Physical delete (`?remove_files=true`) removes the task
  directory including the ledger, so a recreated monitor starts with a
  clean dedupe history (and a fresh baseline).
- **Cursor pagination under concurrent mutation.** The list cursor is the
  previous page's last `task_id`, resolved by position in a freshly
  re-sorted snapshot (`updated_at` desc). If that monitor is mutated
  mid-walk, pagination ends early with an empty page. An opaque
  `(updated_at, task_id)` composite cursor would fix it.
- **Scope-wide update and metrics reads.** `GET /monitor-updates` repeats
  the full-scope record walk and reads each monitor's entire update
  ledger before sorting/truncating — bounded by the 500-record retention
  cap per monitor, but O(monitors × ledger) per request.
  `GET /monitors/{id}/runs` reads one persisted-artifact index per
  execution, newest first, until `limit` accepted runs are found.
  `GET /monitors-metrics` is the heaviest read: the full-scope record
  walk plus one bounded run-ledger read (up to 50 artifact-index reads)
  per monitor — a diagnostics endpoint, not a hot-path list.
