# Unified task panel

A **verdict line** answering "is this okay?", then act cards (Plan, Run,
Output): result first when finished, run first while running. Archived plans:
`docs/archive/plans/2026-10-01-task-drawer-redesign.md`,
`docs/archive/plans/2026-07-29-unified-task-panel-design.md`.

**Every surface that opens a task panel opens this one**: `TaskPanelDrawer`
around `UnifiedTaskPanel`, differing only by **capability**; the panel cannot
tell what kind of task it holds (`rg -i internal UnifiedTaskPanel.svelte`
returns nothing; a test asserts it). `/tasks`, `/tasks?type=internal` and
`/t/<thread>/tasks` now live in Work (`/work`, `/work?type=internal`,
`/work?thread=<id>`, `/work?selected=`); this doc keeps the old names. See
[Unified Work](unified-work.md).

### What is where

Under `src/lib/magician/tasks/` (the internal adapter is
`src/lib/internalTasks/internalTaskPanelModel.ts`); each section names its
module. Pure: `taskVerdict.ts`, `taskCapabilities.ts`, `actSummaries.ts`,
`taskPanelTitle.ts`, `taskRuns.ts`, `taskTimeline.ts`. Components:
`UnifiedTaskPanel.svelte`, `TaskPanelDrawer.svelte`, `TaskActSection.svelte`
(`provenance` prop no longer passed), `TaskVerdictLine.svelte`,
`RunStepDots.svelte`. Adapters: `taskPanelModel.ts`, `executionPanelModel.ts`.
I/O: `taskOutputs.ts`, `taskAttention.ts`, `taskPanelPoll.ts`,
`executionPanelStream.ts`, `taskAsk.ts` (`TaskAsk`, `TaskAskState`,
`answerTaskAsk`). Backend `last_progress_at` (on `TaskState` and the `/v3/tasks`
row): [`docs/components/magician/storage-v2-format.md`](../magician/storage-v2-format.md).

### Work inspector hosting

Work shows the panel as a modal drawer at the page root on every screen size
(full-viewport on phones, with a Work return button; shared focus trap for
Tab/Shift+Tab); the same portal hosts the nested Plan Inspector, so route
overflow clips neither. Thread moves and lookups keep the original task,
scope and credential revision; a scope change or close stops the move, stale
reads are rejected, and a rejected move never shows success. Polling the same
target and scope keeps an open Steer composer draft; a new execution or scope
clears it.

Expanded chat task cards expose the shared Pause/Resume, Steer and Stop controls
for the authoritative active root. Internal tasks resolve that root by task id;
historical run ids remain inspection-only, and collapsed cards make no control
requests.

Approve & Run and task lookups keep their original workspace: switching
workspaces while an answer or lookup is pending cancels the Run request and
asks the reader to reopen the task in its workspace.

### The retirement

`routes/(app)/ExecutionPanel.svelte`, `src/lib/magician/deepwork/` and the
internal raw accordion are gone (`taskPanelSurfaces.test.ts`).
`ExportMenu.svelte` mounts in the drawer's `actions` slot.

### Every surface that opens a task panel

| surface | gesture | adapter |
|---|---|---|
| `/tasks` | task card | `toTaskPanelModel` |
| `/tasks?type=internal` | informational row click | `toInternalTaskPanelModel` |
| `/square` Fleet HUD → Quest Journal | Tasks tile (`TasksWorkspace`) | `toTaskPanelModel` |
| `/t/<thread>/tasks` | card, or `?selected=` | `toTaskPanelModel` |
| `/today` | none: rows link to `/work?selected=<id>` | — |
| chat | `TaskStatusCard` *Inspect run →*, task id in prose, PlannerDock execute | `toTaskPanelModel` |
| chat | `RequestActivityCard` *Inspect run →* | `toExecutionPanelModel` |
| `/crew/<id>` | agent cycle row | `toExecutionPanelModel` |

Chat-next `LiveTaskCard` **Open task →** opens it too
([chat-next.md](chat-next.md#live-task-card)). `/tasks?type=monitors` renders
`MonitorDetailPanel`, **not** a task panel.

**These navigate to `/tasks?selected=` and must keep navigating:** Command
Palette *Open task* and recent-execution rows, `ExecutionControls` *Inspect
run*, `MonitorDetailPanel`'s task link, `PublishedScrollCanvas`,
`VibeLegacyStudio` run rows, Today and Square row clicks. Attention centre and
notification overlay open `openHitlPrompt` → `AttentionPromptModal` or an
`/attention` overlay; `/square?selected=` does nothing. Pinned by
`taskPanelSurfaces.test.ts`.

### Plan steps during a run

The poll applies the current run's (never a history-picked run's)
`debug.selected_execution.step_statuses` via
`taskStore.applyExecutionPanelSteps`; rows show status and the agent's `note`
(`.run-step__note`).

### Off-plan attribution in the Run act

Rows carry `metadata.plan_step_id` (`run.activity_log`), `plan_step_id`
(`debug.timeline`) or `off_plan: true` → `TimelineEntry.planStepId` / `offPlan`,
grouped by `groupActivityByPlanStep` (`planStepActivity.ts`). When any row is
attributed, each step row (`TaskPanelRunStep.stepId`) gets a count opening its
rows, then a folded **Outside the plan · N** (`.run-offplan`), then **All
activity**; otherwise flat. The server also sends
`step_statuses[].activity_count` and `selected_execution.off_plan_activity_count`.
Mobile: iOS `PlanStepActivity` (`PlanProgressModel.swift`), Android
`PlanStepActivity.kt`.

### Why a tool call failed

A failed tool row (`kind: 'tool'`, `tool.failed`) shows the reason under its
`<tool> failed` title: the row's `summary`, else `metadata.error` without the
runtime's `step execution failed:` / `compiled-pack dispatch failed:` prefixes.
It is plain text clamped to two lines, with a Show more / Show less button when
it overflows; other failed rows keep their report rendering. iOS
(`TaskActivityFailure`, `DeepWorkPanel.swift`) and Android
(`TaskActivityCalls.failureDetail`) do the same.

### Plan steps and the Plan header

List rows carry no steps (`convertV3Task` sets `planSteps: []`);
`taskStore.refreshTask` → `ensureTaskPlanStepsLoaded` loads `/v3/tasks/{id}/plan`
once per `latestPlanId:planStatus`, unless planning. Only Plan keeps its header
summary (`approved 15m ago`) while open (`summaryWhenOpen`).

### Plan inspector link

With `canOpenPlan`, a **Plan graph** chip (`mg-chip-action`, `TaskActSection`
`action` prop) sits on the Plan header (Run's when `plan: null`); the panel only
emits `openPlan`. The workspace mounts `PlanInspectorSheet`
(`ExecutionPlanInspector`), mirrored as `/tasks?selected=<task_id>&plan=open`.
Chat passes `canOpenPlan` from its task drawer, and from its Inspect drawer
when the loaded task has a plan or, not loaded, the id is not a synthetic
`agent-cycle:`; both open `openPlanSheet`.

## The verdict function

Pure `deriveVerdict(input): Verdict` → `state`, `headline`, `detail`; every task
surface renders it.

### Priority, not a list

First match wins: `waiting` → `failed` → `stalled` → `running` → `paused` →
`cancelled` → `queued` → `archived` → `finished`. A finished task with an
unanswered question, or one awaiting plan approval, reads `waiting`.

### Waiting on you

`attention` covers all ten `HitlSource` values (`src/lib/hitl/types.ts`) in
human copy, never the enum; a non-blank `attention.summary` wins.
`attention.raisedAt` is **how long you have been blocked** (not `elapsedMs` or
`lastProgressAt`); null or negative omits it.

### Stalled

`running` with no progress for `STALL_AFTER_MS` (5 minutes, inclusive).
`lastProgressAt` is real progress, **never `updated_at`** (rewritten on every
mirrored event). Use the latest task-row progress or current-run model/tool/step
activity, including child work; live observations survive older row polls.
Heartbeats, status updates, metrics and warnings do not advance this clock.
`Task.lastProgressAt` uses
`parseLastProgressAt`, not `parseTimestampToIso`; missing, malformed, `0`, `-1`
are absent. Detail: `Still on step 4: …`, else `Still on this step: …` or
`The run has not advanced`.

### Durations

`45s`; `3m 12s` or `4m`; `1h 20m` or `3h`. Never three units; negative or
non-finite omitted; `0` renders (`Finished · 0s`). Exported:
`durationIfKnown(ms): string | null`, `stepPhrase(step, total)` (`step 4 of 7`).

### `finished` is the fallback

Unmodelled statuses read `finished`; the corpus eval asserts none are real.
Tests: `taskVerdict.test.ts`.

## The capability model

`taskCapabilities.ts`; nothing takes a task kind.

### `deriveActs(caps): ActId[]`

Lifecycle order `plan` → `run` → `output`. **An absent act is absent, never
disabled.** `hasPlanAct` / `hasRunAct` / `hasOutputAct` mean the act applies (no
files reads `no output`; failed to load is absent). Presence is a
`Record<ActId, boolean>`, so a new act won't compile without a capability.

### `actOrder(state, acts): ActId[]` — result first

Output leads for `finished`, `archived`; Run leads for `running`, `stalled`,
`paused`, `failed`, `cancelled`; `waiting`, `queued` keep lifecycle order. Keyed
by `Record<VerdictState, ActId | null>`; a permutation (never adds or drops an
act); an absent leader keeps lifecycle order.
Outside `waiting`/`queued` the leader opens.

### `defaultOpenAct(state, acts, attention): ActId | null`

**An ask outranks the lifecycle**; `attention` (`HitlSource` or `null`) is
required.

| ask / state | opens |
|---|---|
| `plan_approval`, `clarification` | Plan |
| `agentic`, `user_request`, `approval`, `escalation`, `diff_approval`, `service_health`, `bot_auth`, `mcp_oauth` | Run |
| no ask: `queued` | Plan |
| no ask: `running`, `stalled`, `failed`, `cancelled`, `paused` | Run |
| no ask: `finished`, `archived` | Output |

`ACT_TITLES` (`Plan`, `Run`, `Output`) sits beside the type.

### The fallback is directional, not recency

Absent preferred act → nearest earlier, else earliest later (finished, no output
→ Run); `null` only with no acts. Tests: `taskCapabilities.test.ts`.

Header usage and Models share one in-flight scoped query, retaining unknown measurements as unavailable.
Visible completed chat cards load the matching run's bounded report; scope, login and execution checks prevent borrowing another run's document.

## Act summary lines

`actSummaries.ts`. **A summary carries content, not a count.** `planSummary`:
unanswered question (textless → `2 questions waiting`), else approval via
`formatRelativeTime` (`approved 6m ago`, `approved just now`, `approved on Jul 22`;
unreadable → `approved`), else `Record<TaskPlanStatus, string>`
(rejected-after-approval reads `rejected`; unknown status, no questions → empty). `runSummary`:
`14 steps · 2 retries · 217 events · 3m 12s`, live `step 4 of 7 · …`, else
`not started`; steps and events never mix. `outputSummary`: `no output`, one or
two names, or `report.md and 2 images`; `OutputKind` is
`document | image | other`, **no `chart`** (producers can't tell a chart from a
photo). Tests: `actSummaries.test.ts`.

## The act section

`TaskActSection.svelte` is a disclosure ladder: L1 title + summary (summary
**only while closed**, via `MiddleEllipsis`), L2 body (only while `open`), L3
provenance `{label, value}` rows (in **Technical details**). Nothing from level
N+1 is **in the DOM** at level N (`{#if}`, not CSS). The section emits
`toggle`; the panel owns one-open. The header is a full-width `aria-expanded`
button (no `aria-controls`: the body does not exist while closed); cards are not
`native/Card.svelte`. If a poll removes or moves the focused body, focus returns
to the element or the act header; limit: an act that leaves the model takes its
header, so focus has nowhere to go.

## The verdict line

`TaskVerdictLine.svelte` prints both strings verbatim with band and glyph
(colour alone is not a treatment):

| state | band (`data-tone`) | marker |
|---|---|---|
| `waiting` | `attention` | `!` |
| `failed` | `failed` | `✕` |
| `stalled` | `attention` | `⚠︎` (U+FE0E) |
| `running` | `running` | `⟳` |
| `paused` | `neutral` | `Ⅱ` |
| `cancelled` | `neutral` | `■` |
| `queued` | `neutral` | `⋯` |
| `archived` | `neutral` | `◇` |
| `finished` | `completed` | `✓` |

Colour lives only in the `aria-hidden` glyph's dot (`*-soft` token); motion
stops under `prefers-reduced-motion`. Optional `agent`: root of
`run.responsibility.ownerChain`, never guessed. **Not `statusTone()`** (three
states are not statuses); no separate status chip. `role="status"` +
`aria-live="polite"` (not `alert`); detail keeps a `min-height`, wrapping to three
lines for `failed`/attention bands. Known cost: a headline with a duration
re-announces each clock tick. `finished` has `detail: ''`; the panel composes it
from outputs.

## The panel

`UnifiedTaskPanel.svelte` composes the verdict line and present acts, one open,
deciding nothing the pure modules decide; a test renders both task kinds from
fixtures differing in one field and asserts all but the act list match.

### The contract

| prop | |
|---|---|
| `task: TaskPanelModel \| null` | `null` until loaded |
| `loadError: string \| null` | last load/refresh failure |
| `lastLoadedAt: number \| null` | last known-good time |
| `now: number` | required; no `Date.now()` default |
| `outputActions = false` | Open / Reveal on Output rows |
| `filePreviews = false` | caller answers `previewFile` (`TaskPanelDrawer` always does) |
| `filePreview: TaskFilePreview \| null` | expanded row's contents |
| `answerAsk = false` | caller answers `answer` |
| `askState: TaskAskState \| null` | in-flight answer, keyed on ask id |

`TaskPanelModel` = `VerdictInput` minus `now`, plus nullable `plan`, `run`,
`output`, `runs`. **An act is absent when its slice is `null`**;
`output: { files: [] }` is loaded-and-empty. **The panel cannot perform a
request**: each `false` default is offered only when the surface listens;
browser-performed actions (new tab, download, `<img>`) need only a `url`.
`TaskPanelOutput` is `{ files, summary, provenance }`; the `summary` report
renders in full, never clamped (owner decision 2026-10-01). `TaskPanelFile` carries
`name`, `kind`, `path`, `mediaType`, `sizeBytes` and a `url` minted only by
`taskOutputUrl` (`null` removes affordances). Execution file links keep their
segment (`/api/magician/v3/tasks/<task>/outputs/executions/<exec>/outputs/<file>`;
`lib/magician/links/classifyRef.ts`, `classifyRef.test.ts`; task-level
`outputs/<file>` 404s). A served route link — e.g. a delivered file,
`/api/magician/v3/tasks/<task>/outputs/delivered/<exec>/<name>`, any task id —
classifies as `taskOutput` with its path decoded (re-encoded once by
`resolveHref`) and opens through the authenticated fetch; a host path under
`…/[internal_]tasks/<task>/workdir/…` stays `filePath` (host open), since only
`outputs/` is served.

### `id`, and which act is open

`TaskPanelModel.id` tells a different task from another poll. The open act is
`defaultOpenAct(…)` or the caller's `preferredAct` (e.g. `'output'`) until the
reader chooses; the choice survives polls and drops when its act disappears or
`id` changes; clicking the open act closes it. **The id comparison is the only
reset** (no reload, refetch or `{#key}`). Order follows state; the run picker
travels with Run.

### Stopped versus live

Stopped = `finished`, `failed`, `cancelled` (verdict without the ask);
**`stalled` is not stopped**. Live with no files reads `—`, not `no output`;
live Run L1 reads `step 3 of 7`. Finished detail: `Wrote report.md and 2 images`,
`Produced no output`, or empty with no Output act.

### The act bodies

- **Plan**: asks (`HitlPromptFields` when `answerAsk`, else Answer controls),
  then steps.
- **Run**: step rows (label, retries, duration, capability, agent), delegated
  children, responsibility block, recipe cues, timeline.
- **Output**: report, task rows, then per-execution groups (`Direct outputs`,
  `Delegated outputs`, `Persisted artifacts`) in collapsed
  `<details class="output-intermediates">`. Pathless artifacts are metadata only.

### Output rows

Each row: a glyph (short extension, else mime subtype, else `FILE`; image
thumbnail with a `url`), name · `fileMeta` (type · size · mime · produced at),
and chips: `Preview` (if `canPreview`), `In tab`, `Download` (need `url`),
`Open`, `Reveal` (need `outputActions`), named `<label> <file name>`. **Clicking
the name copies the host path** (`copyText`) when `absolutePath` is set; `outputFilesFrom`
takes `absolute_path` only if absolute, and the endpoint sends it only inside the
task's `outputs/`.

**The report's own file sits above the report.** The body prefers the matching
run's bounded source `report`, falling back to that run's summary; summary
verification stays behind the info icon. The source is `primary_user_output_id`,
else the first `audience: "user"` ref;
`fetchTaskOutputFiles` marks that file `reportSource` (`reportSourcePath`), and
it renders above `.output-summary` without `Preview`, not repeated below.

**The primary deliverable leads** (`deliverableOrder.ts`, shared with the chat
card). The outputs row's `primary: true` — or the top-level
`primary_deliverable_output_id` (`withPrimaryDeliverable`; internal tasks read
it off `refs`) — sets `TaskPanelFile.primary`. `orderTaskDeliverables` sorts
task deliverables primary, other delivered files (`user_attachment`,
`user_media`), then the summary (`taskSummary`: role `primary_task_user` or the
generated `out_task_user_…` name), inside the slots deliverables already held,
so non-deliverable indexes do not move. With a primary, the deliverable list
renders **above** the report row and report, the primary row tagged `Main
deliverable`; without one (older tasks) the report stays first. The generated
`out_task_user_…` file is shown as `Summary` (`label`, also used by
`outputSummary`); `name` stays the real file name for downloads.

### Deliverables and other artifacts

The Output act lists **deliverables only**. `GET /tasks/{id}/outputs` marks
`deliverable` (`models::is_deliverable_output`: `audience == "user"` and
`role ∈ {primary_task_user, user_media, user_attachment}`, the Portable Export rule) and lists
sidecars (`task_summary.json`, `voice_speech.json`). `isDeliverableRef`
applies the rule when the field is absent, dropping continuation context and
legacy `out_task_agent_*` copies; no `role` → deliverable unless
`audience: agent`. `outputFilesFrom` marks the rest `deliverable: false` with a
`detail` (`Continuation context`, `Agent copy (legacy)`); run rows
(`execution`/`delegated` scope) are never split. `sidecarFilesFrom` maps sidecars
to `outputs/<file name>` (`detail` e.g. `Task summary`; a name that is not a plain
basename is dropped). `fetchTaskOutputFiles` returns **one
list** (outputs, then sidecars), so `openFile`/`revealFile`/`previewFile` indexes
point into `output.files`; list, summary and `Wrote …` read only
`isTaskDeliverable`.

### Technical details

All provenance (plan ids, `Execution id`, `Task id` / `Agent`, run cost totals,
`Source execution id`, source paths: `outputLocationRows` gives one `Directory`
row when files share a directory, else per-file paths) sits in **one collapsed card at the
bottom**, grouped by act in lifecycle order, absent when no act has rows and
there are no other artifacts. Its toggle is a
`<button aria-expanded aria-controls>`, not `<details>`; it collapses on a new
task id and is sticky across polls. **Other artifacts** (non-deliverables) come
last (`data-tech-group="other-artifacts"`, `fileActions` chips, no `Preview`).

### Output actions

The report renders via `ChatMarkdown` (`sessionId={null}`). Open / Reveal
dispatch `openFile` / `revealFile` (the caller posts `outputs/open-file` /
`outputs/open-folder`); export is `ExportMenu` in workspace chrome. Events carry
`{ file, index }`; **the `TaskPanelFile` is the authority** (`file.path`, never
the basename). Protected URLs cannot carry the bearer, so new tab, download,
thumbnails, `ChatContentBlocks` and markdown output links fetch with auth
(`taskOutputs.ts`) into a short-lived blob URL. HTML opens in a script-disabled
sandboxed iframe (no app storage or opener access); only local protected links
get scoped headers; protected attachments embed as data URLs (usable after the
Work page closes), and a failed attachment closes the viewer with the file
error; the downloaded original is unchanged. MP3 and WAV keep separate
protected open/download actions.

### Reading an output in place

**The drawer answers `previewFile`, not the surface**
(`TaskPanelDrawer.readFilePreview` → `loadOutputPreview`); a test pins that no
surface passes `filePreviews`, `filePreview` or `on:previewFile`. A bare
`UnifiedTaskPanel` keeps the `filePreviews = false` gate (no text Preview chip).
`previewKindOf` covers `markdown`, `image`, `json` (unparseable → text),
`csv` / `tsv` (RFC 4180 table, `PREVIEW_MAX_ROWS` = 200), `code`, `text` / `log`
(monospace, no `shiki`); others get no control. **Mime first, extension
refines**; `application/octet-stream` defers entirely. One open at a time,
fetched on expand. `PREVIEW_MAX_BYTES` = 256 KB on declared size and arrived
**bytes**; over it, offer Open, never truncate. Failures and empty files say so
(`Couldn't read report.md — HTTP 404`, `This file is empty`). `filePreview.index` must match the open
row (`previewFor(filePreview, index)`); `loadOutputPreview` always settles
(`timedFetch`).

### The Run act — what the wire can say about a step

| what | on the wire? | where |
|---|---|---|
| capability | yes | `/v3/tasks/{id}/plan` → `plan_graph.steps[].tool` → `PlanStep.tool_name` |
| delegate agent | yes | `providing_agent_id` |
| delegated subtasks | one level | `/execution-panel` → `run.responsibility.active_children[]` |
| which step delegated a child | no | `ExecutionPanelResponsibilityChild` drops `parent_step_id` |
| per-step duration | no | `PlanStep.duration_ms` never assigned |
| open a step in the event stream | no | no step filter on `/events`; no `step_id` |

`step_statuses` (`capability`, `delegate_agent_id`, `note`, `activity_count`)
carries approved plan steps (`v3_adapter.rs::plan_step_statuses`), read by
`executionPanelModel.ts`.

#### Delegated subtasks

Children follow plan rows as `origin: 'delegated'` (`RunStepOrigin` has two
values, not a depth). **Every step count filters on `origin`**
(`planStepCount`). `delegatedStepsFrom` returns nothing unless
`responsibility.execution_id` is the Run act's execution. `is_blocking` →
`TaskPanelRunStep.blocking`.

#### Who holds the run — the responsibility block

`TaskPanelRun.responsibility`, between steps and timeline; `null` when nothing
was delegated (same execution-id guard).

| row | source | absent when |
|---|---|---|
| `Owner` | last of `owner_chain`, else `active_owner_agent_id`; `→`-joined when longer | neither known |
| `Waiting on` | `waiting_state` via a `Record` | unmodelled |
| `Delegated` | child and blocking counts | never |

Not read (`execution_panel/v3_adapter.rs` hardcodes `handover_active`,
`current_stage`, `current_provider`, `paused_from_state`, `latest_summary`;
`owner_stack` repeats `owner_chain`; `responsibility_summary` is operator
prose). `delegation_chain` is L3 only.

#### Task Recipe rail cues

`recipe.replay` activity keeps a bounded lifecycle payload (kind, version,
duration, step, failure class, origin, transition, approval), never arbitrary
fields or the template, and yields a cue under the verdict
(`Learned API · no browser · 184ms`) plus, with Run open, a recovery narrative.
Recipe ids are never prose.

#### `RunStepStatus`

Seven members including `waiting` (`waiting`/`blocked`/`paused` map there).
`WaitingState` serialises PascalCase; `CHILD_STATUS` matches. Both
`Record<RunStepStatus, …>` maps are total; unmodelled statuses get no mark.

### The three failures that look alike

| task | load error | renders |
|---|---|---|
| present | none | the panel |
| present | present | panel + staleness line (own live region; age via `durationIfKnown`) |
| absent | present | `Can't load this task`, `Retry` (dispatches `retry`), error text |
| absent | none | nothing (drawer skeleton) |

Staleness = a refresh failed while something is shown; no threshold. Tests:
`UnifiedTaskPanel.component.test.ts` (+ `src/test/fixtures/UnifiedTaskPanelHarness.svelte`), `taskFilePreview.test.ts`,
`taskOutputs.test.ts`.

## The store adapter

`toTaskPanelModel(task, outputs, runState)` (`taskPanelModel.ts`) is **the only
place store and panel vocabularies meet**; `verdictStatusOf`, `provenanceRows`,
`outputFilesFrom` are exported for the internal adapter. It settles absence
(`undefined` → `null`; a test fails on any `undefined`), statuses (ten
`TaskStatus` → ten verdict states; `finalizing` reads "Preparing final result" while `synthesis_pending` is set on a finished task, after failed/waiting/cancelled/running/queued/archived and before finished, and falls back to finished after 10 minutes) and what an output file is.

### The status map

`VERDICT_STATUS: Record<TaskStatus, …>`. `paused` reads `Paused · Ready to
resume when you are`; `archived` reads `Archived · No longer active` and is
terminal at every layer (`normalizeV3TaskStatus`, `VERDICT_STATUS`,
`deriveVerdict`, `panelPollCadence`, `taskCapabilities`, the panel's
`SETTLED`), held to the backend's `is_terminal_task_status` by the corpus eval.

| task status | `queuedFor` | second line |
|---|---|---|
| `pending` | `start` | `Not started yet` (drawer: Plan / Run now) |
| `ready` | `approved` | `Plan approved, not started yet` (drawer: Run) |
| `planning` | `plan` | `Working out a plan before it starts` |
| `deferred` | `schedule` | `Scheduled to start later` |
| `queued`, no reason | `null` | `Waiting to start` |

(`verdictStatusOf` / `queuedReasonOf`.) Approving a plan does not start a run;
`POST /v3/tasks/{id}/execute` does. The Plan act shows **planning activity**
(`planProgressFrom`) from `debug.planning_timeline` (older servers:
`debug.timeline` filtered to `planexec_`), folded after planning as
**Planning activity · N events · duration** (open and live, `aria-live`, while
`planning`). `pendingQuestions` are plan-time
clarifications; a mid-run ask ranks `waiting`, a deliberate pause stays `paused`.

### What the adapter refuses to invent

`elapsedMs` `null` (no run-start instant; `createdAt` is write time), plan
question `raisedAt` `null`, step `retries` `0`, step `durationMs` `null` (none
recorded). `attention.source` = `clarification` is a **routing** choice. The
live step is read off the rendered list, not `Task.currentStepIndex`.

### The run state

The third argument is the current run from `/execution-panel` (mid-run ask,
timeline, delegated subtasks); plan questions keep ask priority; `null` = nothing
asking or unknown. `STEP_STATUS` maps the six `PlanStep['status']` values
(unknown → no mark).
`outputFilesFrom(refs, urlFor?)` keeps order, drops pathless rows;
`outputKindOf(mediaType, path)` reads mime first. Tests:
`taskPanelModel.test.ts`, `TasksWorkspace.component.test.ts`.

## The outputs request

`fetchTaskOutputFiles(taskId, principal, workspace)` (`taskOutputs.ts`) →
`GET /api/magician/v3/tasks/{id}/outputs`: `[]`, or `null` on failure or
malformed body (act absent). `taskOutputUrl(taskId, path, principal, workspace)`
puts scope in the **query**, encodes each segment, drops `.`/`..`
(`internalTaskOutputDownloadUrl` does not encode).
`loadOutputPreview(index, file)` answers `ready`, `too-large` or `failed`;
unpreviewable or oversize files make no request.

## The run-state request

`fetchTaskRunState(taskId, principal, workspace, executionId?)`
(`taskAttention.ts`) → `GET /api/magician/v3/tasks/{id}/execution-panel`
(`executionId: null` omits it; no `overview.status` = absence).
`readTaskRunState`: state → `{ ok: true, state }`; `404` or `200` with an
unreadable body → `{ ok: true, state: null }` (no backoff); other non-OK or
thrown → `{ ok: false, reason }`.

`run.needs_attention` is built server-side from the task's full attention list
(list rows, `pendingHitlStore` (no task id) and `/feed/attention` (25 per lane)
cannot). **Filter on `hitl_request`**, else terminal-failure rows read *Waiting
on you*; unmodelled sources are skipped.
`panelRunState` clears only on task id change.

### `awaiting_diff_approval` on the list row

`TaskListItemV3.awaiting_diff_approval` (→ `Task.awaitingDiffApproval`): true
while the run holds a `Pending` `CodeChangeProposal`; derived per read, `false`
when terminal, omitted while false. Chip on `NativeTasksSurface` (`/tasks`,
`/t/<thread>/tasks`, Quest Journal) and `InternalTasksWorkspace`, not
`StudioRail` or `MonitorsWorkspace`; answering still needs the run-state
request. Cockpit runs hold no `chat_session_id`, so
`announce_pending_diff_approvals_to_voice` can't announce them; see
`docs/archive/plans/2026-08-10-vibedev-handoff-phase2.md`.

## Live while it runs

`taskPanelPoll.ts` serves every task-backed surface on `createSharedPoll`
(`src/lib/stores/sharedPoll.ts`: jittered backoff, last good value kept). **One
poller per open panel**: `createTaskPanelPoll({ read, onSnapshot, onFailure })`;
`aim(target, cadence)` replaces the target and tags snapshots.

### Cadence

`panelPollCadence(status)`, a `Record<TaskStatus, …>`: `fast` (4s) for
`running`, `planning`; `idle` (20s) for `paused`, `pending`, `ready`,
`deferred`; `off` for `completed`, `failed`, `cancelled`, `archived` (one read on
aim). `refreshNow()` always reads (Retry, answered asks). The chosen run is part
of the target.

### The verdict moves with the events

Each tick also refreshes the row: `/tasks` via `taskStore.refreshTask(id)` (the
**list route filtered to one id**: the by-id route lacks the plan projection);
internal via `listInternalTasks` (`query` = id, `limit: 1`). Both replace, never
insert. Only the panel payload fails a tick (`onFailure` before re-throw);
`lastLoadedAt` is the panel's last good read, not the list's.
Tests: `taskPanelPoll.component.test.ts`, `taskPanelSurfaces.test.ts`.

## Choosing which run the acts describe

Runs come from `output.recent_runs` (root executions, newest first), not
`GET /v3/tasks/{id}/executions`. `TaskExecutionPanelQuery.execution_id` makes
`V3ExecutionPanelAdapter::get_task_panel_state` scope events, taskplan,
responsibility and outputs to that run **while `overview.status` stays from the
`TaskRecord`**. `/v3/executions/{id}/execution-panel` is not used (404 for
task-backed delegate runs).

### `runsSliceOf(records, selectedId): TaskPanelRuns | null`

Pure over `RunRecord`, used by both adapters (`output.recent_runs` / `/details`
`executions`). `null` with fewer than two runs, a missing or unknown selection,
or no usable record. Labels `#3 · 2 Jul 14:32 · failed` (stable ordinal from
oldest, never an execution id), newest first, in a control before the Run act
(`Run details for [ … ▾ ] of 2 runs`).

### What the selection moves

It moves the Run act (timeline, delegated rows, `Execution id`) and run-owned
Output (`selected_execution_outputs`, `selected_child_outputs`, bounded
`selected_execution_artifacts`; internal: the selected `/details.executions[]`
row), never the verdict, task deliverables or internal `elapsedMs`
(`currentExecution`). A missing run-output projection is unknown, empty is
known-empty, a failed artifact index alone keeps files with a "could not be
loaded" note; artifact paths are sent only relative and
traversal-safe (`executions/<selected-id>/…`). Step marks are the latest
execution's, so `runStepsOf(…, statusesDescribeThisRun)` drops them for earlier
runs; `deriveTimeline` and `delegatedStepsFrom` stay empty until the payload
names the selected execution. The choice is keyed by task id, forgotten on close.
Task surfaces re-read `/execution-panel` with `execution_id`;
`InternalTasksWorkspace` needs no request (`/details` has every execution); chat
*Inspect run* and `/crew/<id>` pass `runs: null`. Tests: `taskRuns.test.ts`,
`taskAttention.test.ts`.

## The drawer shell

`TaskPanelDrawer.svelte` owns everything outside the column: scrim and
`role="dialog" aria-modal="true"` (`aria-label="Task panel"`); focus captured
from `document.activeElement` and restored on close unless that node left the
document; Escape, gated by `closeOnEscape` (`false` while a surface layer is
open); the skeleton while `task === null && loadError === null`; the header card
(controls · move · title · brief · meta); hysteretic condense off body
`scrollTop`; forwarding file/retry/answer events; and answering `previewFile`
(`loadOutputPreview`, request counter, reset on new id).

Header action slots (`.task-panel__header :global(.task-panel__action)`):
`/tasks` Stop / Run / Reset to Ready / Run now / `ExportMenu`; internal
`ExecutionControls`, Stop, Retry synthesis; chat run drawer and crew none.

### Header

- **Title**: `deriveTaskTitle({ label, title, description, id })` shows a real
  title **exactly as passed**. A routing prefix (`<lowercase-slug> / `), or a
  missing, blank or id-shaped title (contains the id, or one whitespace-free
  token ≥ 8 chars with a digit), goes through `titleFromPrompt(description)`:
  strip `slug / `, drop leading `Do not` / `Don't` / `Never` and bare
  `Use (the) browser.` sentences, fold `Use (the) browser … to <verb>` into
  `<Verb>`, keep the first sentence, cap at 90 chars. No prose left → the
  surface's value (`null` → no heading row).
- **Brief**: `deriveTaskBrief` cleans only a prefixed description; hidden when it
  repeats the title. Both clamp to two lines, expandable (a 40vh scroll box
  that outranks condense) only when actually clamped.
- **Id chip**, first in the meta row: monospace, head-and-tail elided, full id
  in `title`, click to copy.
- **Condense:** `nextHeaderCondensed` (`taskPanelHeader.ts`) condenses at
  **≥72px** and expands at **≤12px** (one threshold oscillates); reset on
  `task.id`. `headerChips` shows plan status only (`announce={false}`).
- **Thread mover** (gated by `threadId`, absent with one thread; `Move` only when
  the selection differs): `threadStore.ensureThread` runs before
  `taskStore.updateTask`, so a task never points at an unopenable thread.
  `TasksWorkspace` passes `threadId` and `description`; internal `description`;
  chat and crew neither.

### Layer

`layer` is the scrim `z-index`, default **300**: above the top bar (`200/220`),
below the history drawer (`540/550`) and command palette (`600`). The internal
route passes 1400 (above its hover card 1200 and popover 1300).
`taskPanelPresentation.test.ts` pins it. Tests: `TaskPanelDrawer.component.test.ts`.

## Wired into `/tasks`

`TasksWorkspace.svelte` slots three status-keyed verbs plus **Run now**
(`doit_direct`) via `handleTaskAction`; `escapeBelongsToPanel` lets Escape close
the innermost layer; `panelVisible` requires the task in the list (so no
skeleton here). `now` ticks once a minute. Outputs (keyed on task id + status, a
request counter drops late replies) reach the panel only while known to
describe this task. `panelPoll` aims at `{ taskId, executionId }`; answering an
ask calls `panelPoll.refreshNow()`;
`loadError={$taskStore.error ?? panelLoadFailure}` (`Can't load this task` needs
a real store load failure, not a missing id).

## The internal-task adapter

`toInternalTaskPanelModel(task, details, now, urlFor)` and
`internalTaskOutputFiles(details, urlFor)` map a `/v3/tasks/internal` row
(`TaskListItemV3`; report = `completion_summary`) plus `/details`. Statuses go
through `normalizeV3TaskStatus` (unknown → `pending` → `Queued`). **`plan` is
always `null`** (backend hardcodes `has_plan: false`). `error` and step fields
are `null` and `run.steps` `[]` (executions record only `completed_step_ids`);
`elapsedMs` **is** wired from `started_at`. Before `/details` arrives the verdict
renders alone. The Run act is the named root execution, else the single one
(two or more → `null`), L3 `Execution id` / `Agent`, with no timeline (so no
`runCostRows`). Timestamps use `instantOf` → `parseLastProgressAt` (RFC3339
only), so stalls are detected. `InternalExecutionState` declares `error_message`,
`completion_summary`, `ended_at`, which nothing writes; the end instant is
`completed_at` (`ended_at` fallback). **No mid-run ask**: it surfaces in the spawning chat session.

### Output synthesis

| task state | Output act |
|---|---|
| synthesis pending, nothing written | absent (`Produced no output` would be false) |
| pending, files exist | present with those files |
| synthesis failed | present; verdict carries an ask (`source: 'escalation'` as routing, `raisedAt` = `failed_at`) |
| `refs.outputs` missing or not a list | absent |

Tests: `internalTaskPanelModel.test.ts`.

## Wired into `/tasks?type=internal`

`InternalTasksWorkspace.svelte` opens the drawer (`panelTaskId`) from any
informational part of a row. The panel renders **outside** the row loop, without
`{#key}`, so switching rows keeps the chosen act. `retryInternalTaskSynthesis`
sits in the `actions` slot (it re-spawns an LLM pipeline, unlike the panel's
`retry`); Delete is not slotted. The chosen run is not in the poll target. URLs
from `panelOutputUrlFor`.

## Wired into `/t/<thread>/tasks` and `/today`

`/t/<thread>/tasks` is `TasksWorkspace` with `threadContextId`, reusing
`toTaskPanelModel`, poll and drawer; its `ExecutionPlanInspector` dialog is not a
task panel. `/today` mounts no drawer (rows link to `/work?selected=<id>`).
`taskPanelSurfaces.test.ts` lists `lib/magician/tasks/TasksWorkspace.svelte` as
the only `MIGRATED_ROUTES` entry; `ThreadTasksPanel.component.test.ts` covers the
thread view.

## The execution adapter

`executionPanelModel.ts` maps `ExecutionPanelState` for surfaces holding **a
run, not a task**: no plan → `plan: null`; no fabricated `Task`.

| | chat activity card | `/crew/<id>` cycle |
|---|---|---|
| id | real task id | synthetic `agent-cycle:<agent>:<cycle>` |
| panel endpoint | `/v3/tasks/{id}/execution-panel?execution_id=…` | `/v3/executions/{id}/execution-panel` |
| outputs | `/v3/tasks/{id}/outputs` | none → `files: null`, no Output act |

`executionPanelUrl` owns the branch and requires both ids. Model `id`:
`execution:<id>`, else `task:<id>`. `lastProgressAt` =
latest work event in `run.activity_log`, `run.recent_activity`, `debug.timeline`
(never `overview.updated_at` or a heartbeat). `elapsedMs` = `ended_at − started_at`,
only once ended.
`error`: `debug.latest_error_message`, then
`selected_execution.error_message`, never `run.summary`; `summary` is the report
only. **Ask ranking inverts:** `run.needs_attention` leads,
`run.pending_questions` falls back. Not restored from the retired feed: per-step
`progress` strings, the operator console (`EventStreamCard`, shell output),
per-step open-in-stream.

## The Run act's timeline

`taskTimeline.ts` projects one execution into
`TaskPanelRun.timeline: readonly TimelineEntry[] | null`, minus what acts own
(questions, step statuses, output result).

### One projection, and it owns the `null`

`deriveTimeline(state, executionId)` is the only builder: `null` with no
payload, no execution id, or a payload for **another** execution
(`executionIdOf(state)`), rendering nothing; `[]` renders
`No activity was recorded for this run` (stopped) or `No activity recorded yet`
(live). `output.recent_runs` is filtered to this execution (else retries repeat
`Run started`).

### Render limit

`TIMELINE_RENDER_LIMIT = 200`; `timelineWindow(entries, limit)` returns the
newest rows and the omitted count for a `<p role="note">`
(`Showing the latest 200 of 1,043 events`). `timelineOrigin` and totals read
the full list.

### Delegation envelopes and view modes

Child events group into `.timeline-delegation` envelopes (agent, step count,
status, `delegationSpan(entries, group)`, child feed, a hand-back footer).
`.run-timeline__modes` switches `Grouped` / `Chronological` (with `via <agent>`
badges).

### Compact Run card

- **Closed glance:** `RunStepDots.svelte` in the `glance={true}` slot, one dot
  per grouped-feed position over the whole timeline (`timelineDots`: `done` /
  `failed` / `running` / `waiting`, hollow `unknown`); `timelineDotsView` keeps
  the newest `TIMELINE_DOT_LIMIT = 24` after `+N`, named by one sentence
  (`12 events, 1 failed`) on a `role="img"` row.
- **Open:** newest `TIMELINE_TAIL = 8` (`timelineTail`), `Show all N` /
  `Show latest 8`; reset per task.
- **Fresh rows** (after load) get `data-timeline-fresh="true"` and a one-shot
  `timeline-row-fresh` wash (`animation: none` under reduced motion).
- **Kind tag** after the title; the generic `event` kind renders none.

### When each row happened

Leading column: offset from `timelineOrigin` (`timelineOffset`: `+1m 12s`), with
the wall clock (`timelineClock`, `HH:MM:SS`) as its `title`; no instant, neither.
`--timeline-when: 3.25rem` on `.run-timeline`.

### Row content

Only `shell` entries produce `detail` (machine output: `<pre><code>` with
`Copy`); `body` prose renders via `ChatMarkdown`. Observation title:
`observation.url ?? page_stage ?? 'Observation'`. `TimelineEntry.screenshot` is a
bare boolean (not viewable). `timelineIsolated(entry)` hairlines rows with
`body`/`detail`; only `.output-preview__text` scrolls sideways. `cachedPercent`
divides by `input` alone (`input_tokens` includes cached). A row's duration
stands alone at the trailing edge (no latency bars).

An LLM call is **one row**, titled like the chat card (`llmModelLabel.ts`,
`Thinking with <model>`). `deriveTimeline` pairs `llm.*` rows on
`metadata.llm_call_id` (`llm-call:<id>`) and `tool.*` rows on
`metadata.call_id` per execution (`tool-call:<exec>:<id>`): the entry runs from
the first row, takes the routed model, and the settled row replaces it in
place; without ids a requested row folds into the next settled LLM row. While
the run is live a running row counts up off the shared 1 s clock
(`chat/next/liveClock.ts`).

### What the whole run cost, which the feed stated 70 times and summed nowhere

`runCostRows` sums per-row figures into Run's L3; per-event figures stay in
`timelineCost`. **Client-side from the timeline; no endpoint.** The priced
amount is `metadata.cost_usd` (`metadata.cost` a read-only alias); the UI
**never** prices from a model name.

| row | earned when |
|---|---|
| `Cost` | an LLM event carried a finite, non-negative `cost_usd`; `formatUsd`: `$0.018`, `$3.23`, `$0.000` measured zero, `<$0.001` below half a thousandth (as magios and magdroid) |
| `Tokens` | an event reported input and output counts |
| `Prompt cache` | cache participation |
| `Model time` | something was timed; share needs two instants |
| `Failed calls` | a non-lifecycle row failed (not `Run failed`); dropped at zero |
| `Model` / `Models` | an event named its model |

**Every row is independently earned:** `totalUsage` keeps four nullable figures
and returns `null`, not zeros; model-time share may exceed 100% (unclamped).
`$lib/llm/tokenUsage.ts` owns summation and `cachedPercentOf` (shared with
`RequestActivityCard`), skipping records that reported no figure. Absent on the
internal surface.

### Marks

`TIMELINE_MARKER` borrows `VERDICT_MARKER` for shared statuses (as `STEP_MARKER`
does); `info` is `·`; unmodelled → none.

### Streaming run drawers

`executionPanelStream.ts` serves chat *Inspect run →* and `/crew/<id>` (on the
agent's `current_execution_id`). `ExecutionPanelDelta` is a pushed **full
snapshot**; `panelDeltaState` accepts it only when principal and workspace
match, the body is readable, and **`executionIdOf(state)` equals the target's
execution**. Torn down on run switch and `onDestroy`; a push clears staleness
and moves `panelLastLoadedAt`. `.run-timeline` is `max-height: 22rem`;
`followsBottom` (pure over scroll metrics, all-zero = follow) decides whether to
follow the newest event. Task-backed surfaces poll instead (a delta cannot
match a task); `/tasks?type=internal` has no timeline. Crew pushes use the
`/v3/executions/{id}/execution-panel` builder on the underlying execution (not
`AgentCycleStarted/Completed`), debounced 350 ms.

### Wired into chat

`ChatPanel.svelte` mounts `TaskPanelDrawer` twice, mutually exclusive: **task
drawer** (`toTaskPanelModel`; internal tasks via `taskStore.fetchTaskRecordById`;
Stop / Run / Reset / Run now) and **run drawer** (`toExecutionPanelModel`; no
slot). Both pass `answerAsk`, share one `TaskAskState` (`chatPanelAsk`) with a
request-id guard, and re-read on `attentionStore.resolutions` (`refreshNow`, or
`refreshInspectionInPlace` without blanking).

### Wired into `/crew/<id>`

One drawer, no action slot. `buildAgentCycleTarget` returns
`{taskId, executionId}`; `syncExecutionPanel` returns early when
`cycleKey(target)` equals `executionPanelSubjectKey` (targets rebuild on every
re-hydrate): **clear on subject change, not on request**; the stream callback
re-checks the key.

## The corpus eval

`taskVerdict.corpus.eval.test.ts` (an `/evals` lane, `make test-task-verdict-eval`) asserts
**properties over captured task shapes** through the real adapters: only blocked
shapes read `waiting`, failures carry their reason, no opaque id or `HitlSource`
enum reaches the verdict, every shape has a headline, and without ask and progress the state equals the
status word, so an unmodelled status fails **by name**. Temporary exceptions go
in `KNOWN_UNMODELLED_WIRE_STATUS` (with a staleness assertion). Lane mechanics:
[`docs/components/magician/eval-lanes.md`](../magician/eval-lanes.md).

## Presentation

`taskPanelPresentation.test.ts` asserts these against the stylesheet (theme-wide
rules: [theme-tokens.md](theme-tokens.md)): readable text is only
`--text-secondary` or `--text-primary` (**no third colour tier**: `--text-muted`
and `--text-faint` do not clear 4.5:1 against `--bg-elevated` in the light
themes); no `*-soft` token resolves to its solid
colour; every row-flex item that clips or wraps text has `min-width: 0`; **small
controls are rounded squares, never pills** (`--radius-chip`, 6px, no theme
override); the panel uses no `primary` buttons, and the house
`.native-button--primary` is held to 4.5:1 at every background stop in every
theme except the exempt `arcane-terminal-light`; every
control has hover and a ≥24px target; spacing and radius tokens have **no
fallback** (declared at `:root` in `src/app.css` and every theme) except
`var(--radius-xs, 0.25rem)` (deliberately different fallbacks stay, e.g.
`var(--status-paused-soft, transparent)`). Also asserted:

- Type scale: title `0.95rem` ≥ 1.1× verdict headline (`0.85rem`, 700, the
  boldest) and act title (`0.82rem`, 650); `.panel` body `0.8rem`.
- `--task-panel-bleed` declared once on `.task-panel__body` (readers fall back
  `0px`); `--task-panel-disclosure: 0.75rem` once on `.panel`, no reader fallback.
- `.act__provenance-row` / `.tech-details__row` are `fit-content(40%) minmax(0, 1fr)`
  grids; timeline sub-lines indent with `padding-left`, never `margin-left`.
- No `Card` import; `Retry` / Close are `native/Button.svelte` `outline` `sm`;
  full-width act header and Technical details toggle take `.ui-no-press`.
- Contrast is composited per theme (themes discovered from `app.css`); verdict
  glyph is 60% toward `--text-primary` (3:1 graphic floor).

## Answering an ask — the one renderer

Governed tool approvals keep the server-offered action/whole-tool and
session/always choices; MCP and CLI rows name the operation and the integration.

### Two vocabularies

`HitlInputType` (`src/lib/hitl/types.ts`) mirrors Rust `UserInputType`;
`AttentionPromptKind` (`src/lib/stores/attentionPromptStore.ts`) adds
`multiline`; `PROMPT_KIND: Record<HitlInputType, AttentionPromptKind>` makes a
new input type a build failure. Own shapes: `confirmation` (two labelled
actions), `tool_authorization` (`tool_name`, `params_summary`),
`sandbox_override` (`command`, `violation`, `allowed_roots`), `external_action`
(`instructions`), `file_path` (`multiple`, `filter`); `HitlRequest.hint` says why
a re-ask came back.

`src/lib/hitl/HitlPromptFields.svelte` renders fields; the modal's `Cancel`
means *dismissed*, not *denied*. Per
`PROMPT_HAS_OWN_ACTIONS: Record<AttentionPromptKind, boolean>`, `diff_approval`,
`confirmation`, `authorization` and `external_action` answer only through named
actions (`canSubmit` always `false`), so **Enter cannot grant a capability**. Authorization asks offer every
grant in `options_json` (`allow_once`, `allow_always`, `deny`; default
`allow_once`/`deny`); subject printed verbatim in mono; **the refusal is primary
and takes initial focus**, matched by id, never label. Tests:
`src/lib/hitl/promptFor.test.ts`, `src/lib/hitl/HitlPromptFields.component.test.ts`.

**Approval preview** (`schema.approval_preview`, a confirmation whose gated
tool registered a preview builder —
[hitl-attention.md](../magician/hitl-attention.md#approval-previews)):
`adapters.ts` reads it (line text verbatim, unknown line kinds dropped, the
schema's `action_summary` + `reason` kept as `raw`); `promptFor` sets
`prompt.approvalPreview` and drops `body` (no preview on an alignment hold);
`HitlPromptFields` renders `ApprovalPreviewCard.svelte` above the decision:
title, a `role="alert"` warnings banner, the reason under its label (the
agent's words, text only), `+N lines, −M lines` and the diff through
`DiffStrip`, "… N more changed lines" when capped, evidence, and a collapsed
"Raw call". Without the field the confirmation renders as before. Tests:
`src/lib/hitl/approvalPreview.test.ts`,
`src/lib/hitl/ApprovalPreviewCard.component.test.ts`.

**Preference alignment hold** (`escalation_trigger: "preference_alignment"`):
`promptFor` sets `prompt.alignment`; `HitlPromptFields` renders
`AlignmentAskFields.svelte` (keyed on prompt id) with private detail from
`GET /hitl/{id}/alignment?surface=web` (`alignmentAsk.ts`), never the shared
event. Three unmerged sections (typed choices; "Change what Magician remembers";
reply), nothing preselected, the hold's cancel reads "Cancel the action".
Lasting changes take two steps ("Change my preference", then "Save this wording"
posts the readback token) and hide if the private read fails.
`alignment.also_covers` lists extra reasons ("Allow this once and approve it").
Chat escalation cards carry `input_schema`, so the same prompt opens there.
`mapPromptResultToHitlValue` posts `{type: 'choice', selected_id, other_value?}`
or `{type: 'text'}`. Client
rules: [preference-alignment.md](../magician/preference-alignment.md#owner-surfaces).
Tests: `src/lib/hitl/alignmentAsk.test.ts`,
`src/lib/hitl/AlignmentAskFields.component.test.ts`.

### Answering from the panel

The panel renders and collects; the surface posts. `runAttentionFrom` returns a
`TaskAsk` (attention **and** target from one row):

| adapter | ask → target |
|---|---|
| `toTaskPanelModel` | plan question → the `run.pending_questions[]` row whose `hitl_request.id` equals the question id; mid-run block → the `needs_attention` row's `hitl_request`; draft plan → synthesized (below) |
| `toExecutionPanelModel` | `needs_attention` row / pending question → its `hitl_request` |
| `toInternalTaskPanelModel` | synthesis failure → none; *Retry synthesis* unblocks it |

`askTargetFrom` narrows `source` and `input_type` (on failure: verdict, no
control). Pending asks and plan questions render once in a non-collapsible
**Needs your input** area above the acts, so collapsing Run or opening Output
keeps the questions and any draft answer visible.
`TasksWorkspace`, `/t/{name}/tasks` and both chat drawers pass `answerAsk`.

**Every ask is answerable.** Other plan questions and the run's other
`needs_attention` rows (`moreAsks`, `runAttentionAllFrom`, deduped on id) dispatch
`answer` with `result: null` to open the focused prompt (label: the
`ASK_HANDOFF` string, else `Answer →`); a clarification's matching `<li>` is
suppressed by id while the header counts every question.
`ASK_HANDOFF: Record<HitlInputType, string | null>` hands off `password`,
`tool_authorization`, `sandbox_override` and `diff_approval` (`DiffStrip` needs
an `xl` dialog); `null` (e.g. `confirmation`) answers in place.

**Pending asks the payload does not list.** `TaskPanelDrawer` folds in every ask
`src/lib/attention/pendingAsks.ts` holds for its task id (`metadata.hitl_request`
feed rows included); `mergeTaskPanelAsks` dedupes by every alias and, with no
model `ask`, promotes the newest. Without `answerAsk` (System work, Crew
cycles) the drawer answers via `answerTaskAsk` itself, so a Relentless review or
budget notice is answerable wherever the drawer opens (a `review_href` adds a
`Review →` link that answers nothing). `answerTaskAsk`
substitutes the inline answer for the **first** `openHitlPrompt` modal only
(else an unbounded POST loop); `REASK_LIMIT` (5) bounds re-asks.
The composer dock (chat and VibeDev) lists every open question, wherever the reader is (`composerAsks`), except a notice whose only answer is Dismiss (`isDismissOnlyAsk`: a service-health or expired notice), which stays on the Attention page and the desktop notch: this conversation's first (its chat session, from the ask's `context.chat_session_id` or else the feed row's `input_schema` or `hitl_request.scope`, as Android reads it; a named thread; or its tasks, which in the shared `general` thread means only tasks this chat session started), the rest labelled `From <chat or task>`. Another chat's question is never this conversation's, and the shared `general` thread alone never makes an ask local, as on Android. Only this conversation's question takes typed composer text; one from elsewhere is answered with the dock's own controls.

**No fabricated success:** `TaskAskState` has `sending` and `failed`, no
`answered`. Success clears it and re-reads (the ask leaves when the server stops
listing it); failure keeps it with the reason in `role="alert"`; a mid-answer
scope change says so.

### Approve / reject a plan

`planApprovalAsk` turns a draft plan into an ask (`source: 'plan_approval'`,
`input_type: 'confirmation'`; verdict `Waiting on you · 9m · Approve the plan
before it can run`) with `correlation_id` = plan id, `task_id` =
responder, `raisedAt` = plan update instant; no `latestPlanId` → no ask. **The
wire wins when it speaks**: a plan-approval HITL on the run payload supersedes
it. Replan is `taskStore.replanTask`, a header verb.

`HitlPromptFields` renders **Review plan · Reject · Approve · Approve & Run**
(`planReview: { taskId }`). Approve & Run posts `{confirmation, confirmed: true}`
(choice `approve_run`), then `startRunAfterPlanApproval` (from `respondToHitl`
and `answerTaskAsk`) runs
`taskStore.executeTask` once approval lands; a failed start keeps the approval
and shows a toast. Review dispatches `openPlan`, or expands
`ExecutionPlanInspector` inside `AttentionPromptModal` (widened to `xl`). `PlannerDock` offers
Approve & Run too.

### Delegated-child asks and stalled waits

A child's ask arrives in `run.needs_attention` with
`metadata.delegation_child = true` and `metadata.delegated_agent_id` (plan
`docs/archive/plans/2026-09-30-hitl-surfacing-fixes.md`). `withHitlDelegation`
labels it "from <agent>" and sets `scope.execution_id` to the **child**
execution for `POST /hitl/{correlation_id}/respond`; a flat `correlation_id` is
accepted as the ask id. When `run.waiting_without_question` is set and no ask is
listed, `stalledWaitFrom` yields a `stalledWait` ("Waiting on you, but no
question was delivered") with **Cancel run** (`cancelStalled`); `/tasks` posts
`POST /api/magician/v2/executions/{id}/cancel`, then re-reads.

## Intentional boundaries

- `/crew/<id>` has no Output act: absence beats fabricating `[]`.
- Chat and crew mount wiring is source-contract tested
  (`taskPanelSurfaces.test.ts`); adapters and panel behaviourally.
- Chat task-status cards keep `completion_kind` apart from `completed`: partial
  delivery reads “Partially completed”, pending synthesis “Preparing final
  result”; the kind travels in live events and terminal reconciliation;
  structured cards use a warning tone for partial delivery.

The shared drawer header restores cumulative task usage from the scoped LLM ledger (all executions and descendants), including tokens, cache reads/writes and known cost; missing telemetry remains unreported.
