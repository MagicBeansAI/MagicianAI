# Task detail screen (iOS)

The task detail is one pushed, full-screen page with one scroll, built for a
phone: a header card, the act cards (Output, Run, Plan) in a state-dependent
order with one open, then Technical details. It is the native counterpart of
the web task drawer ([unified-task-panel.md](../unified-ui/unified-task-panel.md));
design §3 of
`docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.

| file | role |
|---|---|
| `magios/Magios/TaskDetailScreen.swift` | the screen, header card, compact title, ⋯ menu, navigation helpers |
| `magios/Magios/TaskDetailActCards.swift` | act cards: Output rows and preview, Run checklist and timeline, Plan steps and planning activity, Technical details |
| `magios/Magios/TaskDetailScreenModel.swift` | `TaskDetailRoute`, the pure rules (`TaskDetailLayout`, `TaskDetailMenu`, `TaskDetailActionRequest`) and the view model |
| `magios/Magios/TaskDetailFixtures.swift` | DEBUG payloads per state for UI tests and screenshots |
| `magios/Magios/PlanApprovalActions.swift` | Review plan · Reject · Approve · Approve & Run, shared by every plan-approval surface |

All task detail entry points use `TaskDetailSnapshot.parse`, which carries the
selected run's plan-step marks (`planStepRows`), planning activity
(`planningEntries`), the plan's steps (`planGraphSteps`, from `/plan`
`plan_graph.steps`) and `planUpdatedAt`.

Task usage merges stored, live and auxiliary model calls by ID within each run and connection scope; unavailable tokens stay unavailable. Real model/tool/child activity advances freshness for the active root, and stale polls cannot move it backward.

## Navigation

Every entry point pushes a `TaskDetailRoute(taskId:executionId:focus:inspectRun:)`
— never a sheet:

| entry | how |
|---|---|
| Tasks list (and internal tasks) | `TasksView` is a `NavigationStack`; `.taskDetailDestination(route)` |
| notifications, `magican://task/{id}`, Today and other `AppActions.requestTask` callers | through the Tasks list's pending-target resolution, so they push too |
| Attention → a plan approval's **Review plan** | opens the [Plan Inspector](plan-inspector.md) (pushed inside the Attention popup's `NavigationStack`), not the task detail |
| chat Steps strip's **Run** (`ActivityTimelineView`) | `@Environment(\.openTaskDetail)`, set by the host's `.taskDetailHost($route)`; Run act open on that run. With no host it opens modally (`TaskDetailSheetHost`) |

A row inside a lazy list cannot declare its own `navigationDestination`; it
calls `openTaskDetail`, and the stack root carries `.taskDetailHost`.

Work hosts the task workspace and its full detail screen; Today, briefing and
Chat use the same destinations. `DeepWorkPanel.swift` keeps compatibility
projection/model helpers but no separate renderer. Manual Work section changes
and New work clear a consumed deep-link target (so Playbooks cannot replay an
earlier Crew detail), and specialist stacks reset when their root section
changes.

## Header card

Verdict line (a `StatusDot` and the verdict from `TaskVerdict`, including
`Queued · Plan approved, not started yet`), the title, a two-line brief (the
description's first paragraph), the recurrence line when it recurs, an id chip
(tap copies the whole id) and a plan chip (`Plan · approved 15m ago`, opens the
Plan act). Once the header's bottom edge scrolls above the top of the scroll
view (`TaskDetailLayout.compactTitleVisible`), a compact title (dot + title)
shows in the navigation bar.

## Waiting on you

Under the header, `TaskDetailAskSection` answers the task's pending asks in
place with the composer ask dock's card (`ComposerAskDock`, `.taskPanel`):
choices, confirm/deny, text and plan approval inline; forms, secrets, grants
and diffs in the focused Attention sheet. Items
(`ComposerAskModel.taskItems`) are the Attention feed's asks owned by the task,
then the execution panel's `needs_attention` asks
(`TaskDetailSnapshot.askRows`) the feed's loaded pages miss, one card per
correlation id. A panel-only ask is vouched for (`vouchPending`) so the
stale-answer guard accepts it; a realtime resolution still wins. An answer
settles the row everywhere (`AttentionViewModel.isSettled`) and re-reads the
panel. **Review draft** opens the ask's Work Quality destination. The Plan
act's questions go to Attention only when this card does not hold them.

## Acts

Order is `TaskActs.actOrder` (result first when finished, run first while
running). One act opens on arrival: an entry point's focus when that act
exists, Run for a run inspection, else `TaskActs.defaultOpenAct`. A closed
card shows its one-line summary (`ActSummaries`); the Plan act keeps its
summary open. Headers are 44 pt tap targets.

- **Output** — deliverables only (`TaskOutputGroups.deliverables`). A row's
  line 1 is the name (middle ellipsis; a tap copies the absolute path, else
  the relative path) and `type · size`; line 2 is **Open · Share · Copy
  path**. Share is a `ShareLink` on the downloaded file once it is on the
  device, else a chip that downloads it and opens the share sheet. The first
  readable deliverable (text, Markdown, JSON, CSV, image) is previewed under
  its own row in a small font, from its snippet until the file downloads. The
  result summary and output-synthesis notices sit above the rows.
- **Run** — the selected run's plan steps as a checklist in plan order
  (`PlanSteps.rows`): ✓ done, ✕ failed, a breathing dot in progress, ○
  pending, ↷ skipped, with the agent's note underneath; after the run a
  pending step reads *not reached*. With off-plan attribution (rows carrying
  `plan_step_id` / `off_plan`), each step shows an activity count that opens
  its rows, and a folded *Outside the plan · N* group follows the steps
  (`PlanStepActivity` in `PlanProgressModel.swift`,
  `TaskDetailActivity.planStepId` / `offPlan`). The run timeline follows
  (latest 30 events, *Show all* up to 200), titled *All activity* when the run
  is attributed. The timeline is one row per call (`TaskActivityCalls.fold`, by
  `metadata.llm_call_id` / `metadata.call_id`, settled in place; without ids
  an `llm.requested` folds into the next settled LLM row), LLM rows titled
  "Thinking with <model>". On a live run a `running` row ticks its elapsed
  time (`TaskActivityLiveClock`). A run picked from Technical details shows its own marks and a
  *Latest run* chip.
- **Plan** — the plan's steps; questions waiting, with *Answer in Attention*;
  a draft plan's Reject · Approve · Approve & Run (Approve & Run approves, then
  starts the run, through `PlanApprovalActions`). Planning activity is a live
  list while planning (milestones with relative times, model calls as compact
  secondary rows with their cost when the row carries `cost_usd`, warnings
  bold), then folds under `Planning activity · N events · duration`. The
  **Plan graph** chip opens the [Plan Inspector](plan-inspector.md) whenever
  the plan has a `plan_graph` (via `@Environment(\.openPlanInspector)`, set by
  the screen's `planInspectorHost`; the Markdown stays in the inspector's More
  menu); a Markdown-only plan keeps the **Plan document** chip. The chat live
  card's Review and Plan graph open the inspector too (the chat stack carries
  `planInspectorHost`).

## Technical details

Collapsed, last: the task's other files (`Other task files` — rows the outputs
endpoint marks non-deliverable and its `internal_files` sidecars — plus the
run's own files and evidence), the run picker / history, and ids (task, run,
plan, agent, thread, chat session, lifecycle).

## ⋯ menu

Live run controls when the task has an active run (Pause, Resume, Steer…,
Stop — `ExecutionControlViewModel`), then `TaskDetailMenu.actions`: Run now,
Plan, Approve / Reject plan, Replan, Retry output synthesis, Reset to Ready,
Mark complete / not done, Cancel task; Publish to Notes for a finished task;
Refresh; Delete (confirmed). Each action is one `TaskDetailActionRequest`
under `/api/magician/v3`; the screen refreshes after it and calls its host's
`onChanged`.

## Live updates

The screen holds a [realtime hub](README.md#realtime-hub) subscription
(`TaskDetailRealtime`, exact execution match; a hub resync refetches) while visible and polls `/execution-panel` and `/plan` every 4 s only
while visible, foregrounded and planning or running
(`TaskDetailLayout.shouldPoll`). A status change seen by the poll triggers a
full refresh so outputs and history follow. Backgrounding stops both;
returning refreshes once.

## Gone

The progress-percentage ring, accent bars, the tab bar and the
"Task deliverables / TASK / RUN" headings. Motion (the breathing dot, the
card and title transitions) stops under Reduce Motion.

## Fixtures and tests

DEBUG: `--open-task-detail-fixture <id>` opens `fixture-detail-planning`,
`-approved`, `-running`, `-done` or `-failed` full screen; with
`--tasks-ui-test-fixture` the Tasks list's fixture tasks load canned payloads.
Unit tests: `MagiosTests/TaskDetailScreenTests` (act order and open act,
Output versus Technical details, poll gate, compact-title threshold, menu and
requests, snapshot fields) and `PlanApprovalActionsTests`.

The bottom tab is **Work**, with no separate Tasks tab. Task and monitor
deep-link parsing accepts saved `/tasks/<id>` and thread-scoped selections,
retaining the exact run and Plan focus; new links use `/work?selected=<id>`. Native task,
plan, output, monitor and Crew detail screens remain.
