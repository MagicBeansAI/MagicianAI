# Task verdict and act capabilities (iOS / Magios)

Source: `magios/Magios/TaskVerdict.swift`, `magios/Magios/TaskCapabilities.swift` ·
tests: `magios/MagiosTests/TaskVerdictTests.swift`, `magios/MagiosTests/TaskCapabilitiesTests.swift`,
plus adapter cases in `magios/MagiosTests/TasksViewModelTests.swift` ·
web originals: `ui/unified-ui/src/lib/magician/tasks/taskVerdict.ts` and `taskCapabilities.ts` ·
design: `docs/archive/plans/2026-07-29-unified-task-panel-design.md`
§2 (the spine, the disclosure ladder, which act opens) and §3 (the verdict).

## What this is, and what it is not

`DeepWorkPanel.swift` is the single task-detail sheet for every entry point (with
a History tab, shell entries, a responsibility card and an execution selector
that web lacks). These two pure modules are its **derivation layer**: they turn
a task's raw state into a one-sentence answer and decide which sections apply.

## The verdict

One sentence answering *"is this task okay?"*, derived from state, attention and
progress. Nine states, checked in a fixed order that **is a priority** — when
more than one is true, the state demanding action wins, so a finished task with
an unanswered question reads *waiting on you*.

| state | line 1 | line 2 |
|---|---|---|
| `waiting` | `Waiting on you · 4m` | the ask's own words, else per-source copy |
| `failed` | `Failed · at step 5 of 7` | the recorded error |
| `stalled` | `Stalled · no progress for 6m` | `Still on step 4: <label>` |
| `running` | `Running · step 4 of 7` | the current step's label |
| `cancelled` | `Cancelled · after 1m 40s` | `You stopped this at step 3` |
| `queued` | `Queued` | `Waiting for a free slot` |
| `paused` | `Paused` | `Ready to resume when you are` |
| `archived` | `Archived` | `No longer active` |
| `finished` | `Finished · 3m 12s` | *(empty — the output summary owns it)* |

The last branch is an **unconditional fall-through**: an unrecognised status
still produces a verdict. New statuses join the chain, not a guard. Only the
`waiting` and `stalled` headlines tick against the clock.

`VerdictState` carries a **marker glyph** (all nine distinct — colour cannot
carry nine states, or any in greyscale) and a **severity band** (shared:
`waiting`/`stalled` are attention; `cancelled`/`queued`/`paused`/`archived` are
neutral). `cancelled` is neutral because a run you stopped on purpose is okay.
Severity is **not** keyed off the panel's `statusColor` helper, which maps task
*statuses*: `stalled` and `finished` are not statuses, and `waiting` would get
the paused colour.

## Where the inputs come from

All inputs are bytes the app already fetches:

| input | source |
|---|---|
| `lastProgressAt` | `state.last_progress_at` in `GET /v3/tasks/{id}` |
| `attention` | `run.needs_attention[].hitl_request` in `GET /v3/tasks/{id}/execution-panel` |
| `currentStep` / `totalSteps` | `overview.current_step` + the step list |
| `elapsed` | the selected execution's `started_at` / `ended_at` |

**`last_progress_at` must never be `updated_at`.** `updated_at` moves on any
write (heartbeat, metric flush, status re-assert), so a wedged run would refresh
its own liveness and never read stalled. The backend moves `last_progress_at`
only on a step transition (`magician/src/magician_v2/artifact_v2/reducer.rs`).

**The attention filter is `hitl_request`, and that is the whole rule.** The
attention list also holds informational rows such as terminal failures; ranking
one as `waiting` would paint a failed task `Waiting on you` and hide its error.
`attentionCount` still counts the whole list for the attention card. An
unmodelled `source` is skipped, and the verdict falls back to what the task's
status earns. The nine HITL sources include `service_health`, whose fallback copy
asks the owner to check service credentials, balance or connection.

## Durations: omit rather than approximate

`TaskVerdict.durationIfKnown` is the **total** function and the only way in.
Missing, negative, non-finite and out-of-range all return `nil` — render no
duration — because each is an unknown wait, not a short one. Negative is
routine: instants come from the server and `now` from the device. `0` still
renders (`0s`).

Durations use at most two units, scaled to the largest (`1h 20m 5s` → `1h 20m`;
`3h`, not `180m`).

**Instants are `Date`, durations are `TimeInterval` (seconds)**, since the panel
already parses wire timestamps into `Date`. The non-finite guard (`Int(exactly:)`,
plus a measurability check on the stall threshold, which `+infinity` would
otherwise clear) is load-bearing: where TypeScript prints `NaNh NaNm`, Swift
**traps**.

## The Run act's timed, live event story

The iOS Run tab reads the same `run.activity_log` contract as web. Every timed
row shows three facts in stable places: a local 24-hour wall clock with seconds,
`+elapsed` from the selected run's recorded start, and the backend's
`metadata.latency_ms`. The app never manufactures a row duration from
neighbours. With no recorded start, the earliest timed event is the origin; a
missing instant or latency is absent, not zero. Plan-step `duration_ms` follows
the same rule.

`llm.*`, `tool.*` and `reasoning*` records keep their canonical metadata and use
web's categories and humanized titles, with bounded inline model/token/cache or
agent context. Run start, end and duration share the summary grid; a running
duration advances from the device clock, a terminal run requires its recorded
end.

The run summary matches web: execution id, exact backend-recorded USD cost,
token and prompt-cache totals, summed model time versus observed span, failed
calls, model attribution. `metadata.cost_usd` is authoritative (legacy
`metadata.cost` accepted for older snapshots). iOS has no price table and never
estimates a missing cost; each row is independently absent when unmeasured, and
a recorded zero shows `$0.00`. Abbreviated tokens round midpoint-away-from-zero
like web (1,250 → `1.3k`).

Rendering is bounded without changing totals: `TaskTimelineFormatting.latest`
keeps the newest 200 rows and the tab says how many of the full set are shown;
the summary, start/end/duration and counts use the complete snapshot.

An open `DeepWorkPanel` owns a scoped connection to
`/api/magician/v2/realtime/ws`. `ExecutionPanelDelta` is a **full snapshot**, not
a patch: iOS replaces activity, steps, questions, attention and other live-run
collections from the latest accepted snapshot, keeping task metadata and
artifacts from their own endpoints. Acceptance is fail-closed on principal,
workspace and the exact selected **execution** id (not task id), so a newer
retry cannot overwrite the historical run being inspected. The socket is live
only while the sheet is visible and reconnects with bounded backoff;
pull-to-refresh is the manual correctness path.

## Acts, and which one opens

`ActId` is `plan → run → output`; `ActId.allCases` **is** the lifecycle order
(web keeps a separate `ORDER` array; `CaseIterable` gives the same guarantee).

`deriveActs` returns only the acts the task has. **An absent act is never
present-and-disabled.** The tab strip is `Overview` + acts in order + `History`;
Overview and History are not acts.

`defaultOpenAct(state:acts:attention:)` **requires the HITL source**: of the
eight `waiting` sources, two are settled in Plan and six are raised mid-run, so
state alone would send a mid-run `diff_approval` to Plan while its control sits
in Run. An ask outranks the lifecycle.

When the target act is absent, **the nearest earlier act opens — else the
earliest later one.** Later acts are empty by definition, so anything behind
beats anything ahead (e.g. a Run act that failed to load between Plan and
Output: Plan wins).

### A missing act is absent, not empty

`hasRunAct` and `hasOutputAct` read the load outcome where it is known: a `nil`
payload passed to `TaskDetailSnapshot.parse` **is** the failed request. A failed
Output act must be absent, because an empty one asserts *no output*. Files that
reached the panel another way (`refs.outputs`, the details payload) still keep
the act.

## The compile guards

Web uses `Record<Union, T>` so adding a union member fails to compile until
valued. **Swift's equivalent is an exhaustive `switch` with no `default`**; a
dictionary would compile with a key missing and return `nil`. Each is a
computed property over an enum:

| web | iOS |
|---|---|
| `ATTENTION_COPY: Record<HitlSource, string>` | `HitlSource.attentionCopy` |
| `ACT_TITLES: Record<ActId, string>` | `ActId.title` |
| `ACT_FOR_STATE: Record<VerdictState, ActId>` | `VerdictState.preferredAct` (`fileprivate`) |
| `ACT_FOR_ATTENTION: Record<HitlSource, ActId>` | `HitlSource.answeredIn` (`fileprivate`) |
| `present: Record<ActId, boolean>` | the `switch` inside `deriveActs` |
| — | `VerdictState.marker`, `VerdictState.severity`, `ActId.detailTab`, `verdictTint` |

`HitlSource`'s raw values are the wire strings, so `HitlSource(rawValue:)` *is*
web's `KNOWN_SOURCES` membership check. Adding a `HitlSource`, `VerdictState` or
`ActId` case fails the build at every corresponding switch.

Tests: both suites run in `make test-ios`.
