# Unified task panel

The task panel is one narrative surface: a **verdict line** that answers "is this
okay?" in a sentence, then act sections (Plan, Run, Output) in fixed order.
Archived design:
`docs/archive/plans/2026-07-29-unified-task-panel-design.md`.

**Every surface that opens a task panel opens this one.** They mount
`TaskPanelDrawer` around `UnifiedTaskPanel` and share its pure modules; what
differs is a **capability**. The panel cannot tell which kind of thing it holds —
not which kind of task, and not whether it is a task at all. If a task-kind
branch ever appears in the panel, the unification has failed
(`rg -i internal UnifiedTaskPanel.svelte` returns nothing, and a test asserts it).

### What is where

All under `src/lib/magician/tasks/` unless noted.

| | |
|---|---|
| `taskVerdict.ts` | the sentence: `(status, ask, error, progress) → state + headline + detail` |
| `taskCapabilities.ts` | which acts a task has, and which one opens |
| `actSummaries.ts` | the line each collapsed act header carries |
| `TaskActSection.svelte` | one act: header always, body while open, provenance behind its own affordance |
| `TaskVerdictLine.svelte` | L0 — the verdict, with a marker glyph and a severity band |
| `UnifiedTaskPanel.svelte` | the column, and everything only a composer can know |
| `TaskPanelDrawer.svelte` | the shell: scrim, dialog, focus, Escape, skeleton, header |
| `taskPanelModel.ts` | store `Task` → panel model |
| `taskOutputs.ts` | the outputs request; the one source of a path and a media type |
| `taskAttention.ts` | the run-state request (mid-run ask + event log in one response) |
| `src/lib/internalTasks/internalTaskPanelModel.ts` | wire row + `/details` → panel model, for `/tasks?type=internal` |
| `executionPanelModel.ts` | `ExecutionPanelState` → panel model, for a **run** (chat activity cards, `/crew/<id>` cycles) |
| `taskRuns.ts` | which of a task's runs the acts describe |
| `taskTimeline.ts` | the Run act's event feed — the only timeline builder, and `executionIdOf` |
| `executionPanelStream.ts` | the run drawer's live subscription |
| `taskPanelPoll.ts` | per-open-panel live refresh for the five task-backed surfaces |
| `taskAsk.ts` | posting an answer: `TaskAsk`, `TaskAskState`, `answerTaskAsk` |
| `taskVerdict.corpus.eval.test.ts` | the `/evals` lane: properties over a corpus |

Backend: `last_progress_at` on `TaskState` and the `/v3/tasks` row; write sites in
[`docs/components/magician/storage-v2-format.md`](../magician/storage-v2-format.md).

### The retirement

`routes/(app)/ExecutionPanel.svelte` and `src/lib/magician/deepwork/` are
deleted; chat and `/crew/<id>` open this panel and the feed is `taskTimeline.ts`
on the Run act. The internal workspace's raw accordion is retired too: row
clicks open this panel. `taskPanelSurfaces.test.ts` asserts both the absence and
the replacement. `ExportMenu.svelte` mounts from the drawer's `actions` slot.

### Every surface that opens a task panel

Anything that opens a task panel opens this one; anything that navigates keeps
navigating.

| surface | gesture | adapter |
|---|---|---|
| `/tasks` | a task card | `toTaskPanelModel` |
| `/tasks?type=internal` | any informational row click | `toInternalTaskPanelModel` |
| `/square` Fleet HUD → Quest Journal | the HUD's Tasks tile (mounts `TasksWorkspace`) | `toTaskPanelModel` |
| `/t/<thread>/tasks` | a card, or `?selected=` | `toTaskPanelModel` |
| `/today` | a source action that created/reused a task, or `?selected=` | `toTaskPanelModel` |
| chat | `TaskStatusCard`'s *Inspect run →*, a task id in prose, PlannerDock execute | `toTaskPanelModel` |
| chat | `RequestActivityCard`'s *Inspect run →* | `toExecutionPanelModel` |
| `/crew/<id>` | an agent cycle row | `toExecutionPanelModel` |

`/tasks?type=monitors` renders `MonitorDetailPanel`, which is **not** a task
panel: it links to `/tasks?selected=`.

**These navigate to `/tasks?selected=` and must keep navigating** — the Command
Palette's *Open task* and recent-execution rows, `ExecutionControls`' *Inspect
run*, `MonitorDetailPanel`'s task link, `PublishedScrollCanvas`,
`VibeLegacyStudio`'s run rows, and every Today or Square row click. The
attention centre and notification overlay reach a HITL prompt
(`openHitlPrompt` → `AttentionPromptModal`) or an `/attention` overlay, never a
task panel. `/square?selected=` does nothing; Square's panel is the Quest
Journal. Pinned by `taskPanelSurfaces.test.ts`.

## The verdict function

`taskVerdict.ts` exports `deriveVerdict(input): Verdict` — pure — returning
`state`, `headline`, `detail`. Every task surface renders what this decides.

### Priority, not a list

First match wins: `waiting` → `failed` → `stalled` → `running` → `paused` →
`cancelled` → `queued` → `archived` → `finished`. The state demanding action
wins — a finished task with an unanswered question is *waiting on you*. The
pair that matters most: a task unstarted because nobody approved its plan is
`queued` *and* asking, and reads `waiting`.

### Waiting on you

`attention` covers all eight `HitlSource` values (`src/lib/hitl/types.ts`), each
with copy written for the person asked; the enum is never rendered. A
backend-supplied `attention.summary` wins over per-source copy; an empty or
whitespace-only summary is treated as absent and falls back to per-source copy.

`attention.raisedAt` is **how long you have been blocked** — distinct from
`elapsedMs` (run duration) and `lastProgressAt`. When null, or when elapsed is
negative, the duration is omitted rather than approximated.

### Stalled

Reported once progress has been silent for `STALL_AFTER_MS` (5 minutes) while
status is `running` (exactly 5 min is stalled). `lastProgressAt` must be a real
progress timestamp, **never `updated_at`**: the realtime bridge rewrites
`updated_at` on every mirrored runtime event, so a wedged run would refresh its
own liveness.

The backend moves `last_progress_at` on step start (including re-activation) and
on step complete/fail; nothing else touches it. `taskStore` exposes it as
`Task.lastProgressAt` via `parseLastProgressAt` — not `parseTimestampToIso`,
which returns *now* for anything unreadable. Missing, malformed, `0` and `-1`
collapse to absent. The wire sends a real timestamp or omits the key. The store
spells absence `undefined`, the pure modules `null`; the coercion lives in the
adapter only.

The detail names the step without its denominator (`Still on step 4: …`);
`currentStep` and `currentStepLabel` are independently nullable (`Still on this
step: …`, or `The run has not advanced`).

### Durations

Scale to the largest useful unit; never three units:

| input | renders |
|---|---|
| `< 60s` | `45s` |
| `< 60m` | `3m 12s`, or `4m` when seconds are zero |
| `>= 60m` | `1h 20m` (seconds always dropped), or `3h` |

A negative or non-finite duration is omitted — `raisedAt`/`elapsedMs` come from
the server and `now` from the browser, so skew makes negatives routine. `0`
renders (`Finished · 0s`). Exported: `durationIfKnown(ms): string | null` and
`stepPhrase(step, total)` (`step 4 of 7`, empty with no step).

### `finished` is the fallback

Any unmodelled status falls through to `finished`. Add new statuses to the
chain, not to a guard; the corpus eval asserts no real status goes unmodelled.

Tests: `taskVerdict.test.ts`.

## The capability model

`taskCapabilities.ts` decides which acts a task has and which opens. It takes no
task-kind parameter, and neither does anything downstream.

### `deriveActs(caps): ActId[]`

Acts in fixed lifecycle order: `plan` → `run` → `output`. **An act the task does
not have is absent, never present-and-disabled.** `hasPlanAct` / `hasRunAct` /
`hasOutputAct` mean *the act applies*, not that it has content: an Output act
with no files is present and summarises `no output`; one that failed to load is
absent. `ActId` is derived from the order array and presence is a
`Record<ActId, boolean>`, so a new act fails to compile until given a
capability.

### `defaultOpenAct(state, acts, attention): ActId | null`

Opens where the story is, not where the lifecycle is. **An ask outranks the
lifecycle**; `attention` (the blocking `HitlSource` or `null`) is required.

| ask | opens |
|---|---|
| `plan_approval`, `clarification` | Plan |
| `agentic`, `user_request`, `approval`, `escalation`, `diff_approval`, `bot_auth` | Run |

| verdict state, no ask | opens |
|---|---|
| `queued` | Plan |
| `running`, `stalled`, `failed`, `cancelled`, `paused` | Run |
| `finished`, `archived` | Output |

`ACT_TITLES` (`Plan`, `Run`, `Output`) lives beside the type.

### The fallback is directional, not recency

When the preferred act is absent, the **nearest earlier** act opens, else the
earliest later one — a later act is empty by definition. A finished task with no
output opens Run; a task blocked on you with no plan opens Run, not an Output it
has not produced. `null` only when there are no acts.

Tests: `taskCapabilities.test.ts`.

## Act summary lines

`actSummaries.ts` writes each collapsed header's L1 line. **A summary carries
content, not a count**; counts remain only as a fallback.

- **`planSummary`** — in priority order: an unanswered question (outranks the
  approval, which would contradict *Waiting on you*; textless →
  `2 questions waiting`), the approval (`approved 6m ago` via
  `formatRelativeTime`; `approved just now` / `approved on Jul 22`; unreadable →
  `approved`), the plan status via a `Record<TaskPlanStatus, string>`. Status is
  the current fact, so rejected-after-approval reads `rejected`. Unknown status
  with no questions renders empty.
- **`runSummary`** — segments joined by `·`, each dropped when empty:
  `14 steps · 2 retries · 217 events · 3m 12s`. While live, the position
  replaces the step count only (`step 4 of 7 · …`). Nothing recorded →
  `not started`. Steps and events never mix: folding events into steps would
  read `step 143 of 217` for a run with no plan.
- **`outputSummary`** — `no output`, one name, two names, or
  `report.md and 2 images`; the named file is the first. `OutputKind` is
  `document | image | other` — **no `chart` kind**, since producers cannot tell a
  chart from a photograph.

Tests: `actSummaries.test.ts`.

## The act section

`TaskActSection.svelte` renders one act as a disclosure ladder:

| level | what | rendered |
|---|---|---|
| L1 | title + summary | the header button, always |
| L2 | act body | default slot, only while `open` |
| L3 | provenance `{label, value}` rows | a `<dl>`, only while its own affordance is open |

Nothing from level N+1 is **in the DOM** at level N (`{#if}`, not CSS). `open`
and provenance are independent flags; closing the act closes provenance. The
section does not open itself: it emits `toggle` with its id and the panel owns
the one-open invariant.

No `Card` and no heading element: three bordered, shadowed rectangles would
fight the hierarchy, and a reusable section cannot know its heading depth. The
header is a hairline rule plus a full-width `aria-expanded` button;
`aria-controls` is omitted because the body does not exist while closed.

When a poll or task swap removes the open body while focus is inside it, the
section hands focus to its own header before the DOM is patched. Unhandled: an
act that disappears from the model entirely takes its header with it.

## The verdict line

`TaskVerdictLine.svelte` takes `verdict: Verdict` and prints both strings
verbatim, with two treatments because colour alone is not one:

| state | severity band (`data-tone`) | marker |
|---|---|---|
| `waiting` | `attention` | `!` |
| `failed` | `failed` | `✕` |
| `stalled` | `attention` | `⚠︎` (U+FE0E, glyph not emoji) |
| `running` | `running` | `⟳` |
| `paused` | `neutral` | `Ⅱ` |
| `cancelled` | `neutral` | `■` |
| `queued` | `neutral` | `⋯` |
| `archived` | `neutral` | `◇` |
| `finished` | `completed` | `✓` |

The glyph (all nine distinct, `aria-hidden`) is what survives colour going away.
The marker colour is pulled 60% toward `--text-primary`.

**Not `statusTone()`.** That maps statuses; three verdict states are not
statuses and would band wrongly. `cancelled` is `neutral` here (a task you
stopped on purpose is okay) while the status chip still shows it as failed.

The detail line always renders with a `min-height`, so the block does not change
height as the task moves. The whole block is `role="status"` +
`aria-live="polite"` (not `alert`: a background poll flipping to failed should
not interrupt). Cost: headlines carrying a duration re-announce on each clock
tick; the fix belongs to the clock owner, not the component.

`deriveVerdict` leaves `detail: ''` for `finished`; the panel composes it (see
below), since this component never sees outputs.

## The panel

`UnifiedTaskPanel.svelte` composes the verdict line and the present acts, one
open. It decides nothing the pure modules decide. A test renders both task kinds
from fixtures differing in exactly one field and asserts everything except the
act list is identical.

### The contract

| prop | |
|---|---|
| `task: TaskPanelModel \| null` | `null` while nothing has loaded |
| `loadError: string \| null` | why the last load or refresh failed |
| `lastLoadedAt: number \| null` | when the state on screen was last known good |
| `now: number` | required clock — no `Date.now()` default |
| `outputActions = false` | Output rows carry Open / Reveal |
| `filePreviews = false` | caller answers `previewFile` |
| `filePreview: TaskFilePreview \| null` | the expanded row's contents |
| `answerAsk = false` | caller answers `answer` |
| `askState: TaskAskState \| null` | in-flight answer keyed on the ask id |

`TaskPanelModel` extends `VerdictInput` minus `now` (so a new verdict field is a
compile error here) plus nullable `plan`, `run`, `output` slices and `runs`.
**An act is absent when its slice is `null`**; `output: { files: [] }` loaded
and found nothing.

The three `false` defaults share one rule: **the panel cannot perform a request.**
Reveal/Open-in-app are POSTs, previews are fetches, answers are posts against a
scope — each is offered only when the surface listens. Browser-performed actions
(new tab, download, `<img>`) need no gate beyond a known `url`.

`TaskPanelOutput` is `{ files, summary, provenance }`; `summary` is the task's
own markdown report (for a prose deliverable, *the* deliverable). A
`TaskPanelFile` carries `name`, `kind`, `path`, `mediaType`, `sizeBytes`, `url`;
a `null` url makes its affordances absent, not broken. `taskOutputUrl` is the
one minter.

### `id`, and which act is open

`TaskPanelModel.id` distinguishes **a different task** from **another poll of the
same task** — both arrive as a new object with opposite meaning.

- With no reader choice, the open act is `defaultOpenAct(…)` and follows state.
- Callers may pass `preferredAct` (e.g. `'output'` from "View Result").
- The reader's choice outlives a poll; clicking the open act closes it ("never
  two open", not "always one").
- A choice is dropped when its act disappears (fall back to state's act) or when
  `id` changes. **The id comparison is the only reset** — not a reload, not a
  refetch, not a `{#key}`; callers must not add a second.
- Order never moves.

### Stopped versus live

`outputSummary([])` is `no output`, premature for a running task where the
honest summary is `—`. The panel decides by **lifecycle**, recomputing the
verdict with the ask removed (a finished task with a question reads `waiting`).
Stopped = `finished`, `failed`, `cancelled`; **`stalled` is not stopped**. The
same predicate turns the Run L1 `step 3 of 7` into `4 steps` and drops the live
marker.

The finished verdict's second line: files → `Wrote report.md and 2 images`;
Output act with no files → `Produced no output`; no Output act → empty.

### The act bodies

- **Plan** — the asks first (with `HitlPromptFields` when `answerAsk`), then the
  plan's steps.
- **Run** — one row per step (label, retries, duration, capability, agent);
  retries nest under their step; delegated children follow the plan rows
  indented; then the responsibility block, recipe cues, and the timeline.
- **Output** — the report first, then stable task deliverables; per-execution
  groups (direct, delegated, persisted artifacts) sit in a collapsed
  `<details class="output-intermediates">`. Structured artifacts with no path are
  metadata only.

Provenance is per act and comes from the act's own slice.

### Output actions

| | |
|---|---|
| report | `ChatMarkdown` with `sessionId={null}` |
| open / reveal | dispatch `openFile` / `revealFile`; caller posts `outputs/open-file` / `outputs/open-folder` |
| new tab / download / image thumb | authenticated fetch, then a short-lived blob URL |
| size and mime | per-row meta, each segment dropped when unrecorded |
| inline preview | per-row disclosure; dispatches `previewFile` |
| export | not in the panel — `ExportMenu` in the workspace chrome |

Events carry `{ file, index }` and **the `TaskPanelFile` is the authority** —
indexing back into the separately fetched list could open the wrong ownership
layer once task/execution/delegated/artifact rows share one list. Handlers use
`file.path`, never the basename. Protected output URLs cannot carry the bearer,
so the Output act and chat's `ChatContentBlocks` fetch bytes via `taskOutputs.ts`
helpers first.

### Reading an output in place

`Preview` expands a disclosure inside the row; the surface fetches and returns
contents on `filePreview`. `previewKindOf`:

| kind | drawn as |
|---|---|
| `markdown` | rendered via `ChatMarkdown` |
| `image` | inline, linking to full size |
| `json` | pretty-printed; unparseable renders as text |
| `csv` / `tsv` | table (RFC 4180, first row header), capped at `PREVIEW_MAX_ROWS` = 200 |
| `code`, `text` / `log` | monospace, no highlighter |
| anything else | no preview control |

`PreviewKind` is its own union, not a widening of `OutputKind`: here the **mime
is read first and the extension refines it** (`text/plain` on a `.csv` is what
producers send); `application/octet-stream` defers entirely.

- Collapsed by default, one open at a time, reset with the chosen act.
- Ceiling `PREVIEW_MAX_BYTES` = 256 KB, checked against the declared size and
  again against arrived **bytes**; over it, say so and offer Open — never
  truncate.
- Fetch on expand, never on render.
- A failure says so (`Couldn't read report.md — HTTP 404`); a truly empty file
  says `This file is empty`.
- `filePreview.index` is checked against the open row, so a stale reply cannot
  render under the wrong row.

No syntax highlighting: `shiki` is async and would make the panel stop being a
pure prop render; plain monospace never lies.

### The Run act — what the wire can say about a step

| what | on the wire? | where |
|---|---|---|
| capability | yes | `/v3/tasks/{id}/plan` → `plan_graph.steps[].tool` → `PlanStep.tool_name` |
| delegate agent | yes | same payload's `providing_agent_id` |
| delegated subtasks | yes, one level | `/execution-panel` → `run.responsibility.active_children[]` |
| which step delegated a child | no | `ExecutionPanelResponsibilityChild` drops `parent_step_id` |
| per-step duration | no | `PlanStep.duration_ms` is assigned by nothing |
| open a step in the event stream | no | no step filter on `/events`; events carry no `step_id` |

`step_statuses` (with `capability`, `delegate_agent_id`) is sent empty:
`build_execution_context` in `v3_adapter.rs` hardcodes `Vec::new()`.
`executionPanelModel.ts` still reads it. Per-step duration renders if present,
but no proportional bar; time is shown on the timeline instead.

#### Delegated subtasks

Delegated children are rows in the **same list** as steps, after all plan rows,
indented, `origin: 'delegated'` (one level; `RunStepOrigin` has two values, not a
depth). **Every step count filters on `origin`** (`planStepCount`); both adapters
build `totalSteps`/`currentStep` before appending delegated rows.
`delegatedStepsFrom` (in `taskPanelModel.ts`, used by both adapters) lists
nothing unless `responsibility.execution_id` equals the execution the Run act
names. `is_blocking` → `TaskPanelRunStep.blocking` (which children still hold
the parent); plan rows are `false`.

#### Who holds the run — the responsibility block

`TaskPanelRun.responsibility`, rendered in the Run act between steps and
timeline; `null` when nothing was delegated, same execution-id guard.

| row | source | absent when |
|---|---|---|
| `Owner` | last link of `owner_chain`, else `active_owner_agent_id`; `→`-joined when longer | neither known |
| `Waiting on` | `waiting_state` via a `Record` | unmodelled state |
| `Delegated` | child count and blocking count | never |

Not read because `execution_panel/v3_adapter.rs` cannot fill them:
`handover_active` (hardcoded `false`); `current_stage`, `current_provider`,
`paused_from_state`, `latest_summary` (hardcoded `None`); `owner_stack` (same
expression as `owner_chain`); `responsibility_summary` (operator prose).
`delegation_chain` is execution ids, so L3 only.

#### Task Recipe rail cues

`recipe.replay` activity keeps a bounded lifecycle payload through the adapter
(kind, version, duration, step, failure class, replayed-step count, origin,
transition target, approval decision) — no arbitrary fields or template. It
yields a cue under the verdict (`Learned API · no browser · 184ms`) and, with Run
open, a plain-language recovery narrative above the timeline (browserless,
healed auth, fell back at a step, recompiled, stopped for a denied write).
Opaque recipe ids are never prose (`the learned API recipe`).

#### `RunStepStatus`

Seven members including `waiting` — work stopped until something acts
(`waiting`/`blocked`/`paused` map here; `pending` would claim it never started).
The wire's `WaitingState` serialises as PascalCase variant names; `CHILD_STATUS`
is keyed accordingly. Both `Record<RunStepStatus, …>` maps must classify every
member. A status the client does not model renders no mark.

### The three failures that look alike

| task | load error | renders |
|---|---|---|
| present | none | the panel |
| present | present | the panel, with a staleness line (its own live region; age via `durationIfKnown`) |
| absent | present | `Can't load this task`, a `Retry` control dispatching `retry`, error text, no verdict or acts |
| absent | none | nothing (the drawer shows its skeleton) |

Staleness means "a refresh failed while something is on screen", not a
threshold.

Tests: `UnifiedTaskPanel.component.test.ts` (+ `src/test/fixtures/UnifiedTaskPanelHarness.svelte`),
`taskFilePreview.test.ts`, `taskOutputs.test.ts`.

## The store adapter

`taskPanelModel.ts` — `toTaskPanelModel(task, outputs, runState)` — is **the only
place the store and panel vocabularies meet**. `verdictStatusOf`,
`provenanceRows`, `outputFilesFrom` are exported for the internal adapter. It
settles three disagreements once: absence (`undefined` → `null`; a test fails on
any `undefined` in a maximally-empty model), status names (ten `TaskStatus`
values → nine verdict states), and what an output file is.

### The status map

`VERDICT_STATUS` is a `Record<TaskStatus, …>`, so a new status is a compile error
(unmapped would silently read `Finished`). `paused` is resumable (`Paused · Ready
to resume when you are`, prefers Run). `archived` is settled (`Archived · No
longer active`, prefers Output) and terminal at every layer —
`normalizeV3TaskStatus`, `VERDICT_STATUS`, `deriveVerdict`, `panelPollCadence`
(`off`), `taskCapabilities`, the panel's `SETTLED` — all held to the backend's
`is_terminal_task_status` by the corpus eval.

One `queued` state, differing second lines via `queuedFor` on the same record
(`verdictStatusOf` / `queuedReasonOf`):

| task status | `queuedFor` | second line |
|---|---|---|
| `pending`, `ready` | `capacity` | `Waiting for a free slot` |
| `planning` | `plan` | `Working out a plan before it starts` |
| `deferred` | `schedule` | `Scheduled to start later` |
| `queued` with no reason | `null` | `Waiting to start` — says less, never something false |

`pendingQuestions` are plan-time clarifications only. An actionable mid-run pause
ranks `waiting` when the execution-panel state carries the ask; a deliberate
pause stays `paused`.

### What the adapter refuses to invent

| field | value | why |
|---|---|---|
| `elapsedMs` | `null` | no run-start instant reaches the store; `createdAt` is when the task was written |
| plan question `raisedAt` | `null` | no timestamp exists; a mid-run ask carries its own |
| step `retries` | `0` | `PlanStep` records none |
| step `durationMs` | `null` | never assigned |

`attention.source` is `clarification` as a **routing** choice (to the Plan act),
not a fact. The live step is read off the rendered step list, not
`Task.currentStepIndex` (0-based, only written for executions this browser
drives).

### The run state

The third argument is the task's current run from `/execution-panel`; from it
come the mid-run ask, the timeline, and delegated subtasks — only this function
knows which execution the Run act names. Plan questions keep priority for the
ask. `null` means both "nothing asking" and "could not find out" (an unread ask
renders nothing).

Per-step status maps the store's six `PlanStep['status']` values through
`STEP_STATUS`; unknown → `null`, no mark.

`outputFilesFrom(refs, urlFor?)` keeps arrival order and drops rows with no path.
`outputKindOf(mediaType, path)`: mime authoritative, extension fallback;
`mediaType` on the file is the raw mime. `urlFor` is a function because only the
caller knows whether the outputs endpoint can answer for its id.

Tests: `taskPanelModel.test.ts`, `TasksWorkspace.component.test.ts`.

## The outputs request

`taskOutputs.ts` — `fetchTaskOutputFiles(taskId, principal, workspace)` against
`GET /api/magician/v3/tasks/{id}/outputs`, the only source with a relative path
and media type per file. Returns `[]` (loaded, nothing) or `null` (failed →
act absent); a malformed body is a failure.

`taskOutputUrl(taskId, path, principal, workspace)` puts scope in the **query**
(the URL is handed to the browser), encodes each path segment, and drops
`.`/`..`. Both `/tasks` routes use it (the older
`internalTaskOutputDownloadUrl` does not encode paths).
`loadOutputPreview(index, file)` answers `ready`, `too-large` or `failed`, and
refuses unpreviewable kinds or oversize files without a request.

## The run-state request

`taskAttention.ts` — `fetchTaskRunState(taskId, principal, workspace, executionId?)`
against `GET /api/magician/v3/tasks/{id}/execution-panel`, returning the whole
payload. `executionId: null` omits the parameter (never `execution_id=`). A body
without `overview.status` is absence.

`readTaskRunState` is the same request with the split a poller needs:

| what came back | answer |
|---|---|
| a panel state | `{ ok: true, state }` |
| `404` | `{ ok: true, state: null }` — no run to describe |
| other non-OK, or thrown | `{ ok: false, reason }` |
| `200` with an unreadable body | `{ ok: true, state: null }` — backend answered; backoff would be misaimed |

**Why this endpoint.** Task-list rows cannot say "blocked mid-run";
`pendingHitlStore` entries carry no task id; `/feed/attention` is scope-wide and
paginated at 25 per lane; a new row field is a backend change.
`run.needs_attention` is built server-side from the full attention list filtered
to this task.

**The filter is `hitl_request`.** Attention rows include terminal failures;
ranking those `waiting` would paint a failed task *Waiting on you*. Unmodelled
sources are skipped (they would render a blank detail).

Loaders must not blank what they have: `panelRunState` is cleared only when the
task id changes.

### `awaiting_diff_approval` on the list row

`TaskListItemV3.awaiting_diff_approval` (→ `Task.awaitingDiffApproval`) is true
while the run holds a `Pending` `CodeChangeProposal`. Server-side: derived from
the proposal store on every read (never from an event), `false` for a terminal
task, omitted from the wire while false. Rendered as a chip beside the status on
`NativeTasksSurface` (`/tasks`, `/t/<thread>/tasks`, Quest Journal) and
`InternalTasksWorkspace`; not on `StudioRail` or `MonitorsWorkspace`. It does not
replace the run-state request — the panel must name, quote and answer the ask.
Cockpit runs hold no `chat_session_id`, so
`announce_pending_diff_approvals_to_voice` cannot announce them; see
`docs/archive/plans/2026-08-10-vibedev-handoff-phase2.md`.

## Live while it runs

`taskPanelPoll.ts` serves all five task-backed surfaces (`/tasks`,
`/tasks?type=internal`, `/today`, `/t/<thread>/tasks`, chat's task drawer); each
aims its own instance.

### Shape

Built on `createSharedPoll` (`src/lib/stores/sharedPoll.ts`: jittered
exponential backoff, fast lease, last good value kept). **One poller per open
panel, aimed at a target it closes over** — a poller per (task, run) would leak
one per switch (no destructor), and a module singleton would let two mounted
routes steal each other's target. `createTaskPanelPoll({ read, onSnapshot,
onFailure })` lives as long as the component; `aim(target, cadence)` replaces
the target, and snapshots are tagged with it so late replies are recognisable.

### Cadence

`panelPollCadence(status)` is a `Record<TaskStatus, …>`, keyed on task status
(the verdict folds `planning` and `pending` together, but only one is working):

| cadence | statuses |
|---|---|
| `fast` (4s) | `running`, `planning` |
| `idle` (20s) | `paused`, `pending`, `ready`, `deferred` |
| `off` | `completed`, `failed`, `cancelled`, `archived` |

`off` still reads once — a newly aimed target always reads, and on `/tasks` this
poll is the payload's only source. `refreshNow()` reads regardless of cadence
(Retry, answered asks). The chosen run is part of the target, so it is asked for
on every tick and selecting a run makes no request of its own.

### The verdict moves with the events

The verdict reads the **task record**, so each tick refreshes the row too:
`/tasks` calls `taskStore.refreshTask(id)` (the **list route filtered to one
id** — the by-id route lacks the plan projection; `loadTasks()` would reconvert
everything and flip `isLoading`); `/tasks?type=internal` re-reads via
`listInternalTasks` with `query` = id, `limit: 1`, list filters not sent. Both
replace, never insert (an upsert would leak internal tasks into `/tasks`).

Only the panel's own payload can fail the tick. On failure `onFailure` runs
before re-throwing; the surface shows the staleness line over what is on screen.
`lastLoadedAt` is the panel's last good read, not the list's.

Tests: `taskPanelPoll.component.test.ts`, `TasksWorkspace.component.test.ts`,
`taskPanelSurfaces.test.ts` (five surfaces share this module, no second interval).

## Choosing which run the acts describe

A task can run more than once. Runs come from `/execution-panel`'s
`output.recent_runs` (root nodes of the execution tree, newest first; id, start,
outcome) — not `GET /v3/tasks/{id}/executions`.

**No backend change was needed.** `TaskExecutionPanelQuery.execution_id` makes
`V3ExecutionPanelAdapter::get_task_panel_state` scope the event log, taskplan,
responsibility snapshot and output result to that run **while `overview.status`
stays read off the `TaskRecord`** — which is what lets a task-level verdict sit
over an execution-scoped act. `/v3/executions/{id}/execution-panel` is not used
here: it is not populated for task-backed delegate runs and fails with 404.

### `runsSliceOf(records, selectedId): TaskPanelRuns | null`

Pure, over a neutral `RunRecord` (id, instant, status word through
`verdictStatusOf`); both adapters call it (`output.recent_runs` / `/details`'s
`executions`). `null` — so no control, never a disabled one — when fewer than two
runs, no selection, the selection is not among the runs (a `<select>` would show
the first), or no record is usable.

Labels are `#3 · 2 Jul 14:32 · failed` — ordinal (counted from the oldest, so
stable), day+clock, outcome; empty segments drop, the ordinal never does, and no
label contains an execution id. Newest first; both orders derived from instants.

The control renders inside the act `{#each}`, immediately before the Run act
(`Run details for [ … ▾ ] of 2 runs`): the header is already five rows, above
the verdict it would seem to scope the whole panel, inside the Run body it would
vanish when collapsed.

### What the selection moves

- **Moves:** the Run act (timeline, delegated rows, `Execution id`), and the
  run-owned half of Output — `selected_execution_outputs`,
  `selected_child_outputs`, and bounded `selected_execution_artifacts` for
  `overview.execution_id` (internal adapter: the selected `/details.executions[]`
  row).
- **Does not move:** the verdict (task record) and task deliverables
  (task-scoped). Internal `elapsedMs` keeps reading `currentExecution`.

Missing run-output projection = unknown (no group); empty = known-empty; a
failed artifact index alone leaves files visible with a "could not load" note.
The artifact projection sends identity, type, content type, production time,
optional size, and a path only when relative and traversal-safe (normalised to
`executions/<selected-id>/…`); unsafe/structured artifacts are metadata only.

Plan step marks are the *latest* execution's, so `runStepsOf(…,
statusesDescribeThisRun)` drops status and duration for an earlier run (the
per-run `debug.selected_execution.step_statuses` exists but is sent empty).

The selection is reader intent held beside the payload: `deriveTimeline` and
`delegatedStepsFrom` return nothing until the payload names the selected
execution, so during the round trip they are absent rather than wrong. Re-picking
the shown run dispatches nothing; the choice is keyed by task id and forgotten
on close.

| surface | how the choice is served |
|---|---|
| `/tasks`, `/today`, `/t/<thread>/tasks`, chat task drawer | re-read `/execution-panel` with `execution_id` |
| `InternalTasksWorkspace` | no request — `/details` carries every execution |
| chat *Inspect run* drawer, `/crew/<id>` | `runs: null` — the surface is one execution |

Tests: `taskRuns.test.ts`, `taskAttention.test.ts`, `UnifiedTaskPanel.component.test.ts`.

## The drawer shell

`TaskPanelDrawer.svelte` owns everything outside the panel column, once:

| | |
|---|---|
| scrim + `role="dialog" aria-modal="true"` | `aria-label="Task panel"` |
| focus | captured from `document.activeElement` on mount, restored on close unless that node left the document |
| Escape | closes, gated by `closeOnEscape` (surfaces pass `false` while their own layer is open) |
| loading skeleton | `task === null && loadError === null` |
| header | five rows |
| thread mover | when `threadId` is non-null |
| header condense | hysteretic, off the body's `scrollTop` |
| panel mount | file/retry/answer events forwarded |

It does **not** own the header's actions — surfaces slot them (`/tasks`: Stop /
Run / Reset to Ready / Run now / `ExportMenu`; internal: `ExecutionControls`,
Stop, Retry synthesis; chat run drawer and crew: nothing). Their look comes from
`.task-panel__header :global(.task-panel__action)`.

### Header

Rows top-down: **controls** (task id leading, actions + Close trailing) ·
**move** · **title** (optional) · **description** · **chips**.

- Title and description clamp by lines (needs `-webkit-line-clamp` and
  `line-clamp`, `display: -webkit-box`, `-webkit-box-orient: vertical`,
  `overflow: hidden`); full text on `title=`. They are buttons that exist only
  when actually clamped (`scrollHeight` vs `clientHeight`, `ResizeObserver`); the
  overflow flag latches; expanding becomes a 40vh scroll box and outranks the
  condense.
- The task id is present at the header (a deliberate exception to "identifiers
  are L3" — correlating with logs needs it): elided head-and-tail, full value in
  `title`, click to copy.
- **Condense:** scrolling drops the title to one line and removes the
  description. `nextHeaderCondensed` (`taskPanelHeader.ts`) condenses at
  **≥72px** and expands at **≤12px** — one threshold self-oscillates because
  removing the description shortens the content and scroll anchoring pulls back
  across the line. Reset keyed on `task.id`. Reduced-motion is handled in this
  file (`app.css`'s guard does not reach it).
- **Chips** (`headerChips`): the status as a word and the plan status — neither
  restates the verdict. `announce={false}`: the verdict is the one live region.
- **Thread mover:** `threadId` gates it (the model carries no thread); absent
  with one thread; `Move` appears only when the selection differs.
  `threadStore.ensureThread` runs before `taskStore.updateTask` so the task never
  points at an unopenable thread. `TasksWorkspace`, `/today`,
  `/t/<thread>/tasks` pass `threadId` and `description`; internal passes
  `description`; chat and crew neither.

### Layer

`layer` is the scrim `z-index`, default **300**: above the top bar (`200/220`),
below the history drawer (`540/550`) and command palette (`600`). The internal
route passes 1400 to cover its hover card (1200) and message popover (1300).
`taskPanelPresentation.test.ts` pins the default against those components.

Tests: `TaskPanelDrawer.component.test.ts`.

## Wired into `/tasks`

`TasksWorkspace.svelte` slots a subset of `NativeTasksSurface`'s ladder (three
status-keyed verbs plus **Run now** / `doit_direct`) routed through
`handleTaskAction`. `escapeBelongsToPanel` lets Escape close the innermost thing.

- The skeleton cannot fire here: `panelVisible` requires the task in the list.
  (Only a `?selected=` deep link on the internal route reaches the blank body.)
- `now` ticks once a minute, slower than the poll.
- Outputs are keyed on task id + status, and a request counter drops late
  replies; files reach the panel only while known to describe this task.
- `panelPoll` is aimed at `{ taskId, executionId }`;
  `loadError={$taskStore.error ?? panelLoadFailure}`.
- Preview writes its `loading` record before the await.
- Answering an ask calls `panelPoll.refreshNow()`.
- `Can't load this task` requires an actual store load failure, not merely a
  missing id.

## The internal-task adapter

`internalTaskPanelModel.ts` — `toInternalTaskPanelModel(task, details, now,
urlFor)` and `internalTaskOutputFiles(details, urlFor)` — maps a
`/v3/tasks/internal` row plus `/details`.

- Rows are `TaskListItemV3`, so the report is `completion_summary` off the row.
- Raw wire statuses reach `verdictStatusOf` only through `normalizeV3TaskStatus`
  (unknown → `pending` → `Queued`, not `Finished`).
- **`plan` is always `null`** (backend hardcodes `has_plan: false`).
- Before `/details` arrives, the verdict renders alone.
- `error`, step progress fields: `null`; `run.steps`: `[]` (executions record
  `completed_step_ids`, identifiers not descriptions).
- `elapsedMs` **is** wired (execution records carry `started_at`); that is why
  the adapter takes `now`.
- The Run act describes the root execution the task names; with none named, a
  single execution is the run, two or more → `null`. L3 rows: `Execution id`,
  `Agent`. Timeline is `null`, so `runCostRows` is honestly absent.
- Timestamps go through `instantOf` → `parseLastProgressAt` (RFC3339 only);
  `last_progress_at` is on the row, so stall detection works.
- `InternalExecutionState` declares `error_message`, `completion_summary`,
  `ended_at` but the record writes none; the end instant is `completed_at`, with
  `ended_at` as fallback.
- **No mid-run ask here**: this surface's only attention is failed output
  synthesis; a mid-run HITL surfaces in the spawning chat session.

### Output synthesis

| task state | Output act |
|---|---|
| synthesis pending, nothing written | absent (else `Produced no output` would be false) |
| pending, files exist | present with those files |
| synthesis failed | present, and the verdict carries an ask (`source: 'escalation'` as routing, `raisedAt` = `failed_at`) |
| `refs.outputs` missing or not a list | absent |

Tests: `internalTaskPanelModel.test.ts`.

## Wired into `/tasks?type=internal`

`InternalTasksWorkspace.svelte` opens the drawer from any informational part of
a row; native controls (bulk-select, retry, Stop, Delete) keep their own
actions; the drawer's task is `panelTaskId`. The panel
renders **outside** the row loop (and without `{#key}`) so switching rows does
not recreate it and reset the chosen act. `retryInternalTaskSynthesis` sits in
the `actions` slot — distinct from the panel's `retry` (refetch), since it
re-spawns an LLM pipeline. Delete is not in the slot. Polling uses the same
module; a tick buys steps, outputs and verdict (no event log); the chosen run is
not in the target. Output actions and preview work as on `/tasks`, with URLs
minted by `panelOutputUrlFor`.

## Wired into `/t/<thread>/tasks` and `/today`

The thread route holds a real store `Task`, so `toTaskPanelModel` is reused
unchanged with the same loaders, clock, poll and drawer.

- **`/t/<thread>/tasks`** slots three verbs + Run now via
  `handleThreadTaskAction`; its `ExecutionPlanInspector` dialog is a plan
  inspector, not a task panel.
- **`/today`** no longer mounts the drawer since the Morning Edition cutover:
  its task rows link to `/tasks?selected=<id>`, which opens the panel there.

`taskPanelSurfaces.test.ts` lists the thread route as the only
`MIGRATED_ROUTES` entry; `ThreadTasksPanel.component.test.ts` proves it
behaviourally.

## The execution adapter

`executionPanelModel.ts` maps `ExecutionPanelState` onto `TaskPanelModel` for the
two surfaces that hold **a run, not a task**. A run with no plan has
`plan: null`; no fabricated `Task`.

| | chat activity card | `/crew/<id>` cycle |
|---|---|---|
| id | a real task id | synthetic `agent-cycle:<agent>:<cycle>` |
| panel endpoint | `/v3/tasks/{id}/execution-panel?execution_id=…` | `/v3/executions/{id}/execution-panel` |
| outputs | `/v3/tasks/{id}/outputs` | none → `files: null`, Output act absent |

`executionPanelUrl` owns that branch. Both ids are required: synthesising
`agent-cycle:<execution>` would hit the execution-scoped route, which is empty for
task-backed delegate runs.

- `id` names the run: `execution:<id>`, else `task:<id>`.
- `lastProgressAt` = max over `run.activity_log`, `run.recent_activity`,
  `debug.timeline` — never `overview.updated_at`.
- `elapsedMs` = `ended_at − started_at`, only once ended.
- `currentStep` is read off the rendered step list.
- Unmodelled step status words render no mark (the wire's `status` is a bare
  string with several spellings of "done").
- `error`: `debug.latest_error_message`, then `selected_execution.error_message`;
  never `run.summary`.
- `summary` is the written report only; `outcome` is not a fallback.
- **Ask ranking inverts:** `run.needs_attention` (canonical for this run) leads,
  `run.pending_questions` is the fallback; its `clarification` source routes via
  the directional fallback since there is no Plan act.

Not restored from the retired feed: per-step `progress` strings, the operator
console (`EventStreamCard`, Plan Inspector, observations, shell output, linked
inputs), per-step open-in-stream.

## The Run act's timeline

`taskTimeline.ts` projects one execution's payload into
`TaskPanelRun.timeline: readonly TimelineEntry[] | null` — what the run did,
excluding the kinds the acts already own (questions, step statuses, output
result; a test feeds all three and expects `[]`).

### One projection, and it owns the `null`

`deriveTimeline(state, executionId)` is the only builder; both adapters call it.
`null` (not `[]`) when there is no payload, no execution id, or the payload
describes **another** execution (the task adapter's id and payload come from
separate reads). `executionIdOf(state)` is read only to disagree with the
caller's id, never to substitute for it.

| `timeline` | body renders |
|---|---|
| `null` | nothing |
| `[]`, stopped | `No activity was recorded for this run` |
| `[]`, live | `No activity recorded yet` |
| entries | the feed |

Lifecycle rows carry no execution id (L3 provenance has it);
`output.recent_runs` is filtered to this execution, else a thrice-retried task
shows three `Run started` rows.

### Render limit

`TIMELINE_RENDER_LIMIT = 200`. `timelineWindow(entries, limit)` returns the
newest rows and the exact omitted count; above the limit a `<p role="note">`
outside the `<ol>` says `Showing the latest 200 of 1,043 events`. Only DOM growth
is bounded: `latencyScale`, `timelineOrigin` and run cost/token totals read the
complete list.

### Delegation envelopes and view modes

Delegated child events are grouped into `.timeline-delegation` envelopes: a
summary (`Delegated to <agent>`, step count, status, span from
`delegationSpan(entries, group)`), the child's feed indented, and a footer
(`↳ Handed back results to main agent · 10:14:45 (43s)`) or active/failure
indicator. Semantic tokens only. `.run-timeline__modes` switches `Grouped` and
`Chronological` (flat, with `via <agent>` badges).

### When each row happened

Each row's leading column stacks a wall clock (`timelineClock`: local
`HH:MM:SS`, no date) over an offset (`timelineOffset`: `+1m 12s` from
`timelineOrigin`, the earliest recorded instant in the feed, computed once).
Missing timestamps are coalesced to `0` = "no instant recorded"; such rows show
neither (no 1970 clocks). The origin row reads `+0s`. `--timeline-when: 3.5rem`
is declared on `.run-timeline`.

### Row content

| what | element | treatment |
|---|---|---|
| machine output (`detail`) | `<figure>` › `<pre><code>` | recessed, mono, `Copy` control |
| event prose (`body`) | `.timeline-row__body` | markdown via `ChatMarkdown` |
| model | `.timeline-row__id` | `<code>` |
| cost/tokens/cache | `.timeline-row__meta` | tabular figures, second line |
| latency | `.timeline-row__latency` | end of title row, fixed 4rem track |

Nothing sniffs strings; only `shell` entries produce `detail`. An observation
title is `observation.url ?? page_stage ?? 'Observation'` and the entry does not
record which. `Copy` confirms on the copied block (one id) and clears after two
seconds. `TimelineEntry.screenshot` is a bare boolean — the observation id is not
carried, so screenshots are not viewable.

`timelineIsolated(entry)`: rows with `body` or `detail` get a hairline bound
above and below (no inset, no wash); the cost line is not a trigger. Neither the
feed nor a stdout block scrolls sideways (`overflow-x: hidden`, `pre-wrap`,
`overflow-wrap: anywhere`); `.output-preview__text` is the one intentional
horizontal scroller.

Token counts and latency are L2, not provenance. `cachedPercent` divides by
`input` alone because this provider's `input_tokens` already includes the cached
portion.

**Latency bars:** a 2px rule under each latency, scaled against the **longest
call** (not the total), drawn only once two rows are timed, all full when all
equal, with a floor so a fast real measurement still shows; `aria-hidden`.
`latencyScale` is computed once per feed.

### What the whole run cost, which the feed stated 70 times and summed nowhere

Every row carries its own model, tokens and latency, so a long run states its
cost many times and its total nowhere. `runCostRows` sums them into the Run act's
L3 — act-scoped facts about **this execution**; per-event figures stay at L2 in
`timelineCost`. **Derived client-side from the timeline; no endpoint.** `runOf`
derives the timeline once and reads it for rows and totals. The projection
exposes the runtime's priced amount as `metadata.cost_usd` (`metadata.cost` is a
read-only alias); the UI sums recorded amounts and **never** prices from a model
name.

| row | earned when |
|---|---|
| `Cost` | some LLM event carried a finite, non-negative `cost_usd`; a measured zero renders `$0.00` |
| `Tokens` | some event reported both input and output counts |
| `Prompt cache` | the run had cache participation |
| `Model time` | something was timed; share needs a feed spanning two instants |
| `Failed calls` | a non-lifecycle row failed (excludes the `Run failed` row); dropped at zero |
| `Model` / `Models` | any event named its model |

**Honest absence is the correctness risk; every row is independently earned.**
`totalUsage` keeps four separately nullable figures and returns `null` rather
than zeros. Model-time share can exceed 100% (concurrent calls) and is not
clamped. `$lib/llm/tokenUsage.ts` owns summation and `cachedPercentOf` (shared
with `RequestActivityCard`); a record is skipped when it reported no figure, not
when figures summed to zero. `Task id` is not here — the header has it. On the
internal surface the timeline is `null`, so the sum is absent.

### Marks

`TIMELINE_MARKER` borrows `VERDICT_MARKER` by reference for shared statuses (as
`STEP_MARKER` does); `info` is `·`; unmodelled → no mark, gutter kept.

### Streaming run drawers

`executionPanelStream.ts` is the only bridge for chat's *Inspect run →* and
`/crew/<id>`. `ExecutionPanelDelta` is a server-pushed **full snapshot** — no
accumulator. `panelDeltaState` accepts a delta only if principal and workspace
match, the body is a readable panel state, and **`executionIdOf(state)` equals
the target's execution** (matching on task id would accept a sibling
execution's deltas). Keyed on the run, torn down on run switch and `onDestroy`.
A push clears staleness and moves `panelLastLoadedAt`.

`.run-timeline` is `max-height: 22rem; overflow-y: auto` so following the newest
event does not drag Output off screen. `followsBottom` is a pure function over
the scroll metrics (all-zero = keep following).

| surface | timeline | kept current by |
|---|---|---|
| chat *Inspect run →* | yes | stream |
| `/crew/<id>` | yes | stream, on the agent's `current_execution_id` |
| `/tasks`, `/today`, `/t/<thread>/tasks`, chat task drawer | yes | `createTaskPanelPoll` |
| `/tasks?type=internal` | no | `/details` projects no event log |

Task-backed surfaces poll because their subject is a **task**, which
`panelDeltaState` deliberately cannot match; they trade sub-poll latency. For
crew, the synthetic cycle id is only an endpoint selector; the projector builds
pushed state with the same builder as `/v3/executions/{id}/execution-panel`,
deltas come from the underlying execution (not `AgentCycleStarted/Completed`),
debounced at 350 ms.

### Wired into chat

`ChatPanel.svelte` mounts `TaskPanelDrawer` twice, mutually exclusive:

- **task drawer** — `toTaskPanelModel`; internal tasks resolve through
  `taskStore.fetchTaskRecordById`; slot: Stop / Run / Reset / Run now.
- **run drawer** — `toExecutionPanelModel`, panel state + outputs under one
  request id; no action slot.

Neither passes `answerAsk`; each would need its own `TaskAskState`, request-id
guard and refresh, and the run drawer has no poll to `refreshNow`.

### Wired into `/crew/<id>`

One drawer, no action slot (every retired verb acted on a task; no task endpoint
answers for `agent-cycle:`). `buildAgentCycleTarget` returns
`{taskId, executionId}`. `syncExecutionPanel` compares `cycleKey(target)` with
`executionPanelSubjectKey` and returns early when equal — the target object is
rebuilt on every agent re-hydrate, so **the clear belongs to a subject change,
not a request**; the stream callback re-checks the same key.

## The corpus eval

`taskVerdict.corpus.eval.test.ts` is an `/evals` lane
(`make test-task-verdict-eval`) asserting **properties over captured task shapes**
of both kinds, replayed through the real adapters: only blocked shapes read
`waiting`, failures carry their reason, no opaque id or `HitlSource` enum reaches
the verdict, every shape has a headline. Its main job: with ask and progress
removed, the state must equal the status word, so an unmodelled status fails
**by name** instead of falling through to `finished`. Temporarily unmodelled
wire statuses go in `KNOWN_UNMODELLED_WIRE_STATUS`, with a staleness assertion.
Lane mechanics: [`docs/components/magician/eval-lanes.md`](../magician/eval-lanes.md).

## Presentation

Computed style is not observable from a render, so
`taskPanelPresentation.test.ts` asserts these against the stylesheet text.

### Type scale

| level | | size | weight | face | colour |
|---|---|---|---|---|---|
| L0 | verdict headline | `1.125rem` | 700 | display | `--text-primary` |
| L1 | act title | `0.9375rem` | 600 | display | `--text-primary` |
| chrome | drawer heading (`<h2>`) | `0.8125rem` | 500 | body | `--text-secondary` |

L0 and L1 share a colour so only type ranks them — the hierarchy must survive
themes that collapse every wash to one grey. `.panel__load-error` matches L0.

Timeline rows: **the title carries the row's only weight** (500); kind is an
uppercase `0.625rem` micro-label; when/latency/meta are `0.6875rem` tabular
`--text-secondary`; stdout is `--text-primary` on `--bg-soft`. **No third colour
tier**: `--text-muted` is 2.98:1 on the default light surface, so the lower tier
is spelled in size, tracking and case.

### Layout

- Output rows are bounded (`1px solid var(--border-soft)`, `--radius-sm`,
  `--space-sm`); timeline rows are not — a feed is hundreds of homogeneous
  events, an output list a few deliverables.
- Sections run edge to edge: `--task-panel-bleed` is declared once on
  `.task-panel__body` and each section pads by it (fallback `0px` = a panel
  outside a drawer). The verdict band has no radius. `act--open` takes the hover
  wash on the header only.
- `--task-panel-disclosure: 0.75rem` is declared once on `.panel`; readers write
  no fallback.
- **Flex floor:** every item of a row-direction flex container that clips or
  wraps text declares `min-width: 0` (the test classifies every container).
  `.act__provenance-row` is a grid `7rem minmax(0, 1fr)` instead, because
  `min-width: 0` plus `overflow-wrap: anywhere` collapses a path to zero width.
  Timeline sub-lines use `padding-left`, never `margin-left` (which overflows a
  `flex-basis: 100%` box).
- Output Details: `outputLocationRows` gives one `Directory` row when files share
  a directory, else per-file paths; a flat listing yields none.

### Controls

Every control (`Details`, `Preview`/`Open`/`Reveal`, `In tab`/`Download`,
`Retry`, Close) is `native/Button.svelte` `outline` `sm`; the ask's submit is
`secondary` `sm`. No leftover panel rules for them (`className` lands inside the
component, so such rules match nothing). `Button` supports
`ariaExpanded`/`ariaControls` (`null` omits the attribute) and
`href`/`target`/`rel`/`download` (renders an `<a>`). Not adopted: `Card` (test
asserts no import, no `box-shadow` on output rows); the act header (a full-width
row `Button` cannot be; it takes `.ui-no-press`, since `app.css`'s
`scale(0.96)` press would squeeze a 560px row by ~21px); the surfaces' slotted
`.task-panel__action` buttons. The `primary` variant is not used (fails 4.5:1 in
seven themes). Every control has a hover state and a ≥24px target.

### Contrast

The test resolves each theme's custom properties from `app.css` (themes
discovered, not listed) and composites washes over the drawer surface. Readable
text is only `--text-secondary` or `--text-primary`; provenance values move up to
`--text-primary`. Retry counts use
`color-mix(in srgb, var(--status-attention) 50%, var(--text-primary))`; the
verdict glyph 60% (3:1 graphic floor). No `*-soft` token may resolve to its own
solid colour (`--accent-tertiary-soft` is `color-mix(… 16%, transparent)`).

### Tokens without fallbacks

Spacing and radius tokens are written with **no fallback**: they are declared at
`:root` in `src/app.css` and by every theme, so a fallback would be an unchecked
second copy (one, `var(--radius-xs, 0.25rem)`, remains). Fallbacks naming a
deliberately different value stay (`var(--status-paused-soft, transparent)`,
`var(--accent-primary, currentColor)`).

## Answering an ask — the one renderer

### Two vocabularies

`HitlInputType` (`src/lib/hitl/types.ts`) mirrors Rust `UserInputType`;
`AttentionPromptKind` (`src/lib/stores/attentionPromptStore.ts`) describes a
form and adds `multiline` for non-HITL callers (editing task descriptions).
They are not collapsed; `PROMPT_KIND: Record<HitlInputType, AttentionPromptKind>`
makes a new input type a build failure. Shapes of their own:
`confirmation` (two labelled actions), `tool_authorization` (`tool_name`,
`params_summary`), `sandbox_override` (`command`, `violation`, `allowed_roots`),
`external_action` (`instructions`), `file_path` (`multiple`, `filter`).
`HitlRequest.hint` is carried — on a re-ask it is why the question came back.

`src/lib/hitl/HitlPromptFields.svelte` is the shared field renderer; the modal
keeps only dialog chrome (title, eyebrow, overlay registration, chord, and a
`Cancel` meaning *dismissed*, not *denied*).

`PROMPT_HAS_OWN_ACTIONS: Record<AttentionPromptKind, boolean>`: `diff_approval`,
`confirmation`, `authorization`, `external_action` answer through named actions,
so `canSubmit` is permanently `false` for them and **Enter cannot grant a
capability**.

**Authorization block** (`tool_authorization`, `sandbox_override`): subject
printed verbatim in mono, wrapping; every grant the ask offers is a control
(`options_json` gives `allow_once`, `allow_always`, `deny`; default
`allow_once`/`deny`); **the refusal is primary and takes initial focus**, matched
by id, never label.

Tests: `src/lib/hitl/promptFor.test.ts`, `src/lib/hitl/HitlPromptFields.component.test.ts`.

### Answering from the panel

The panel renders and collects; the surface posts. `runAttentionFrom` returns a
`TaskAsk` — the verdict's attention **and** the target from one row (derived
apart, they can disagree).

| adapter | ask | target |
|---|---|---|
| `toTaskPanelModel` | plan question | `run.pending_questions[]` whose `hitl_request.id` equals the question id (identifier match) |
| `toTaskPanelModel` | mid-run block | the `needs_attention` row's `hitl_request` |
| `toTaskPanelModel` | draft plan | synthesized (below) |
| `toExecutionPanelModel` | `needs_attention` row / pending question | that row's `hitl_request` |
| `toInternalTaskPanelModel` | synthesis failure | none — no correlation id; *Retry synthesis* unblocks it |

`askTargetFrom` narrows `source` and `input_type`; a failed narrowing leaves the
verdict (`Waiting on you`) with no control.

The ask renders once, at the top of `askAct` = `defaultOpenAct` over the ask's
source (derived from `autoAct`, not `openAct`, so it does not follow the reader).
A plan clarification's matching `<li>` is suppressed by id; the header still
counts every question. Only `TasksWorkspace` passes `answerAsk`.

`ASK_HANDOFF: Record<HitlInputType, string | null>` — `null` answers in place;
a string labels a control opening the focused prompt. Handed off: `password` (a
secret goes in something opened to type it), `tool_authorization` and
`sandbox_override` (grants happen where refusal is primary),
`diff_approval` (`DiffStrip` needs an `xl` dialog). `confirmation` answers in
place.

**Re-ask loop:** `answerTaskAsk` calls `openHitlPrompt` with a `respond` that
substitutes the inline answer for the **first** modal only; re-asks reopen the
prompt with the revised question. `REASK_LIMIT` in `openHitlPrompt` bounds it —
without the first-only substitution the exchange becomes an unbounded POST loop.

**No fabricated success:** `TaskAskState` has `sending` and `failed`, no
`answered`. Success clears it and re-reads; the ask leaves when the server stops
listing it. Failure keeps the ask with the reason in `role="alert"`. State is
keyed on the ask id; a mid-answer scope change reports *"Your scope changed while
this was being answered — reopen the task to check."*

### Approve / reject a plan

A draft plan normalises to `pending` → `queued` and would read `Waiting for a free
slot`. `planApprovalAsk` makes it an ask (`source: 'plan_approval'`,
`input_type: 'confirmation'`, Approve/Reject in place; verdict `Waiting on you ·
9m · Approve the plan before it can run`). The target is factual:
`correlation_id` is the plan id, `task_id` the responder identity, `raisedAt` the
plan record's update instant; no `latestPlanId` → no ask. **The wire wins when it
speaks**: a plan-approval HITL on the run payload supersedes this fallback, which
exists because such tasks usually have no live execution. Replan is not an ask
(no `HitlSource`); it is `taskStore.replanTask`, a header-level verb.

## Intentional boundaries

- `/crew/<id>` has no Output act: no endpoint answers outputs for an
  `agent-cycle:` id, and absence beats fabricating `[]`.
- Chat and crew mount wiring is source-contract tested in
  `taskPanelSurfaces.test.ts` (they need hydrated runtime state); their adapters
  and the shared panel are behaviourally tested at their seams.
- The newest 200 timeline rows render with an explicit notice; run-level counts
  and durations use the complete event set.
