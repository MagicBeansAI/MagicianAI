# Task verdict and act capabilities (iOS / Magios)

Source: `magios/Magios/TaskVerdict.swift`, `magios/Magios/TaskCapabilities.swift` ·
tests: `magios/MagiosTests/TaskVerdictTests.swift`, `magios/MagiosTests/TaskCapabilitiesTests.swift`,
plus adapter cases in `magios/MagiosTests/TasksViewModelTests.swift` ·
web originals: `ui/unified-ui/src/lib/magician/tasks/taskVerdict.ts` and `taskCapabilities.ts` ·
design: `docs/archive/plans/2026-07-29-unified-task-panel-design.md`
§2 (the spine, the disclosure ladder, which act opens) and §3 (the verdict).

## What this is, and what it is not

These two pure modules are the **derivation layer** of the task detail
([`TaskDetailScreen`](task-detail.md)): they turn a task's raw state into a
one-sentence answer and decide which acts apply. `DeepWorkPanel.swift` holds
the shared detail models (`TaskTimelineFormatting`, `TaskRunSummary`); its
legacy `DeepWorkPanel` view has no remaining callers.

## The verdict

One sentence answering *"is this task okay?"*, from state, attention and
progress. Nine states are checked in a fixed order that **is a priority**: the
state demanding action wins, so a finished task with an open question reads
*waiting on you*.

| state | line 1 | line 2 |
|---|---|---|
| `waiting` | `Waiting on you · 4m` | the ask's own words, else per-source copy |
| `failed` | `Failed · at step 5 of 7` | the recorded error |
| `stalled` | `Stalled · no progress for 6m` | `Still on step 4: <label>` |
| `running` | `Running · step 4 of 7` | the current step's label |
| `cancelled` | `Cancelled · after 1m 40s` | `You stopped this at step 3` |
| `queued` | `Queued` | why it has not started: `Not started yet` (pending), `Plan approved, not started yet` (ready), `Working out a plan before it starts` (planning), `Scheduled to start later` (deferred); `Waiting to start` when no reason is known |
| `paused` | `Paused` | `Ready to resume when you are` |
| `archived` | `Archived` | `No longer active` |
| `finished` | `Finished · 3m 12s` | *(empty — the output summary owns it)* |

The last branch is an **unconditional fall-through**, so an unknown status
still gets a verdict. Only the `waiting` and `stalled` headlines tick with the
clock.

`VerdictState` carries a **marker glyph** (all nine distinct, so state never
rests on colour) and a **severity band**: `waiting`/`stalled` are attention;
`cancelled`/`queued`/`paused`/`archived` are neutral (a run you stopped is
okay). Severity is **not** keyed off the `statusColor` helper, which maps task
*statuses*; `stalled` and `finished` are not statuses.

## Where the inputs come from

All inputs are bytes the app already fetches:

| input | source |
|---|---|
| `lastProgressAt` | `state.last_progress_at` in `GET /v3/tasks/{id}` |
| `attention` | `run.needs_attention[].hitl_request` in `GET /v3/tasks/{id}/execution-panel` |
| `currentStep` / `totalSteps` | `overview.current_step` + the step list |
| `elapsed` | the selected execution's `started_at` / `ended_at` |

**`last_progress_at` must never be `updated_at`.** `updated_at` moves on any
write (heartbeat, metric flush), so a wedged run would never read stalled.
Only the app-recipe reducers in `magician/src/magician_v2/artifact_v2/reducer.rs`
set `last_progress_at` (run start, step start, claim renewal, step completion,
output adoption, cancellation). A task without it never reads `stalled`
(`TaskVerdict.swift` checks stall only when `lastProgressAt` is present).

**The attention filter is `hitl_request`, and that is the whole rule.** The
list also holds informational rows such as terminal failures, which must not
read as `Waiting on you`. `attentionCount` still counts the whole list. An
unmodelled `source` is skipped and the verdict falls back to the status. The
ten HITL sources include `service_health` (fallback copy asks the owner to
check service credentials, balance or connection) and `mcp_oauth` (MCP server
sign-in), answered in Run. Legacy `primitive` and `inner_loop` fold to
`agentic`, as in web `normalizeHitlSource`.

Each parsed ask carries a `targetID` (the attention row's `id`, else its
`hitl_request.identifiers.correlation_id`); "Review in Attention" opens that
ask's sheet (`TaskDetailSnapshot.attentionTargetID`, falling back to the first
pending clarification's id). With neither, Attention opens the list.

## Durations: omit rather than approximate

`TaskVerdict.durationIfKnown` is the **total** function and the only way in.
Missing, negative (server instant vs device clock), non-finite and
out-of-range values return `nil`, and no duration renders. `0` renders as `0s`.
At most two units, scaled to the largest (`1h 20m 5s` → `1h 20m`; `3h`, not
`180m`).

**Instants are `Date`, durations are `TimeInterval` (seconds).** The
non-finite guard (`Int(exactly:)`, plus a measurability check on the stall
threshold) is load-bearing: where TypeScript prints `NaNh NaNm`, Swift
**traps**.

## The Run act's timed, live event story

The Run act reads the same `run.activity_log` contract as web. Every timed row
shows a local 24-hour wall clock with seconds, `+elapsed` from the run's
recorded start, and the backend's `metadata.latency_ms`. The app never derives
a row duration from neighbours. With no recorded start, the earliest timed
event is the origin; a missing instant or latency is absent, not zero.
Plan-step `duration_ms` follows the same rule.

`llm.*`, `tool.*` and `reasoning*` records keep their canonical metadata and
use web's categories and titles, with bounded inline model/token/cache or agent
context. A running duration advances from the device clock; a terminal run
needs its recorded end.

The run summary matches web: execution id, backend-recorded USD cost, token and
prompt-cache totals, summed model time versus observed span, failed calls,
model attribution. `metadata.cost_usd` is authoritative (legacy
`metadata.cost` accepted). iOS has no price table and never estimates a missing
cost; each row is absent when unmeasured, and a recorded zero shows `$0.000`.
`TaskTimelineFormatting.usd` (web `formatUsd`) shows three decimals below a
dollar (`$0.005`), two from a dollar up (`$3.23`), and `<$0.001` below half a
thousandth (VoiceOver: "less than a tenth of a cent"). Abbreviated tokens round
midpoint-away-from-zero like web (1,250 → `1.3k`).

`TaskTimelineFormatting.latest` renders the newest 200 rows and says how many
are shown; summary, start/end/duration and counts use the full snapshot.

An open [`TaskDetailScreen`](task-detail.md) subscribes to
`ExecutionPanelDelta` on the app's realtime hub
(`/api/magician/v2/realtime/ws`). It is a **full snapshot**, not a patch: iOS
replaces activity, steps, questions, attention and other live-run collections,
keeping task metadata and artifacts from their own endpoints. Acceptance is
fail-closed on principal, workspace and the exact selected **execution** id
(not task id), so a newer retry cannot overwrite the run being inspected. The
subscription lives only while the screen is visible; a hub resync refetches.
Pull-to-refresh is the manual path, and the screen also polls
`/execution-panel` every 4 s while visible and planning or running.

## Acts, and which one opens

`ActId` is `plan → run → output`; `ActId.allCases` **is** the lifecycle order
(web keeps a separate `ORDER` array; `CaseIterable` gives the same guarantee).

`deriveActs` returns only the acts the task has. **An absent act is never
present-and-disabled.** `TaskDetailScreen` renders them as cards in
`TaskActs.actOrder` (result first when finished, run first while running).

`defaultOpenAct(state:acts:attention:)` **requires the HITL source**: of the
ten `waiting` sources, two are settled in Plan and eight mid-run, so a mid-run
`diff_approval` must open Run. An ask outranks the lifecycle.

When the target act is absent, **the nearest earlier act opens, else the
earliest later one** (later acts are empty by definition).

### A missing act is absent, not empty

`hasRunAct` and `hasOutputAct` read the load outcome: a `nil` payload passed
to `TaskDetailSnapshot.parse` **is** the failed request. A failed Output act is
absent, because an empty one asserts *no output*. Files that arrived another way
(`refs.outputs`, the details payload) still keep the act.

## The compile guards

Web uses `Record<Union, T>` so a new union member fails to compile until
valued. **Swift's equivalent is an exhaustive `switch` with no `default`** (a
dictionary would compile with a key missing). Each is a computed property over
an enum:

| web | iOS |
|---|---|
| `ATTENTION_COPY: Record<HitlSource, string>` | `HitlSource.attentionCopy` |
| `ACT_TITLES: Record<ActId, string>` | `ActId.title` |
| `ACT_FOR_STATE: Record<VerdictState, ActId>` | `VerdictState.preferredAct` (`fileprivate`) |
| `ACT_FOR_ATTENTION: Record<HitlSource, ActId>` | `HitlSource.answeredIn` (`fileprivate`) |
| `present: Record<ActId, boolean>` | the `switch` inside `deriveActs` |
| — | `VerdictState.marker`, `VerdictState.severity`, `ActId.detailTab`, `verdictTint` |

`HitlSource`'s raw values are the wire strings; `HitlSource.normalized(_:)`
(trim, canonical raw value, legacy alias) *is* web's `normalizeHitlSource` /
`KNOWN_HITL_SOURCES` membership check. Adding a `HitlSource`, `VerdictState` or
`ActId` case fails the build at every corresponding switch.

Tests: both suites run in `make test-ios`.
