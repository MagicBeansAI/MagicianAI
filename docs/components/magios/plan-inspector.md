# Plan Inspector (iOS)

A full-screen native port of the web Plan Inspector
(`ui/unified-ui/src/routes/(app)/ExecutionPlanInspector.svelte`,
`PlanGraphView.svelte`, `SlotGraphInspector.svelte`), designed for a phone.
Approved design: §7 of
`docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.
It opens from a task's plan; the task detail screen is
[task-detail.md](task-detail.md).

| file | role |
|---|---|
| `magios/Magios/PlanGraphModel.swift` | `PlanGraph` / `PlanStep` / `PlanEdge`, `PlanJSON` (lossless JSON), levels, waterfall order, layered layout, cycle-safe dependency helpers, edit operations, save payload |
| `magios/Magios/PlanInspectorClient.swift` | request building and response reading for every endpoint below, behind `PlanInspectorTransport` |
| `magios/Magios/PlanInspectorModel.swift` | `PlanInspectorRoute`, the view model (state rules, edit/save, history/restore, panes, clarifications, approval, live marks) |
| `magios/Magios/PlanInspectorScreen.swift` | the screen, toolbar, approval bar, presentation helpers (`planInspectorHost`, `planInspectorDestination`, `PlanInspectorSheetHost`, `@Environment(\.openPlanInspector)`) |
| `magios/Magios/PlanGraphCanvas.swift` | the zoomable DAG and its node cards |
| `magios/Magios/PlanStepsView.swift` | waterfall cards, the edit list, shared tags/cards/JSON disclosure |
| `magios/Magios/PlanStepDetailSheet.swift` | one step: read-only fields, or editors in edit mode |
| `magios/Magios/PlanInspectorMoreViews.swift` | History, Analysis, Slots, Attempts, Clarifications |
| `magios/Magios/PlanInspectorFixtures.swift` | DEBUG canned responses (`--open-plan-inspector-fixture`) |

## Views

- **Header:** status, steps, edges, unresolved inputs, confidence, strategy.
- **Graph** (default): a layered top-to-bottom DAG, not the web's force
  layout. A step's level is 1 + the deepest of its dependencies (0 for a
  start step); the first row keeps waterfall order and every later row is
  ordered by the mean x of each node's parents, which cuts edge crossings.
  Pinch to zoom (0.3–3×), drag to pan, zoom buttons and Fit. Nodes show a
  confidence fill (≥ 70 % green, ≥ 40 % amber, else red), a dashed border
  when no tool is resolved, ▶ on a start step, "via <agent>" when delegated,
  and — unlike the web — the live run mark from `/execution-panel`
  `debug.selected_execution.step_statuses` (`PlanSteps.rows`, matched by
  step id, else by position). The panel is read once on open and every 4 s
  while a run is active, the screen visible and the app active.
- **Steps:** the web waterfall (Kahn with depth-first child placement):
  number, level, confidence, tool ✓ / ⚠, depends-on with reasons, up to
  three parameters. Tap opens the step sheet (`.medium` / `.large`).
- **History:** the latest 20 saved versions; tap previews read-only;
  Restore asks first (destructive), then the plan and versions reload and
  Steps shows.
- **More:** Analysis (query analysis JSON), Slots (grouped by type, largest
  first, with the group's mean confidence; clarified task and the other
  snapshot parts as JSON), Attempts (confidence, outcome, resources and
  exploration JSON), Clarifications (pending questions with option chips or
  a typed answer, one request per question; the answered history; Advanced
  → manual resume while planning or eliciting), the plan document when the
  caller passed one, and Replan for a draft, rejected or failed plan.
- **Draft plan:** a bottom bar Reject (confirmed) · Approve · Approve & Run.
  Approve & Run approves, then posts `/execute`; a failed start keeps the
  approval and says so (`PlanApprovalActions.approveAndRun`).

## Editing

Open for plan status `draft`, `eliciting` and `approved` (the web unlocked
approved plans so a tweak does not round-trip through draft). Edit enters a
draft copy sorted into waterfall order. Steps becomes a list: hold and drag
to reorder, swipe to delete, Add step (`step_<epoch ms>`, "New step",
confidence 0.5). The step sheet edits task, confidence (0–1 in 0.05 steps),
tool id (free text — the web's tool picker is fed by an empty list and no
tool-catalog endpoint backs it), parameters, expected outputs, metadata,
start step, and dependencies through a picker that offers only steps that
cannot make a cycle (not itself, not an existing dependency, none of its
transitive dependents). Dependency changes re-sort into waterfall order.

Save asks for an optional label and sends `PUT /plan {plan, label}`. Edges
are rebuilt from the dependencies, each keeping its old reason, else
"dependency". **Nothing the server sent is dropped:** each step and the graph
keep the JSON object they were decoded from and only the edited fields are
written over it, so `session_policy`, `planning_metadata`,
`response_contract`, `success_criteria`, `parameter_provenance`, `role`,
`readiness`, `sub_steps` and typed (non-string) parameters round-trip; a
step's `depends_on` is written to match the rebuilt edges. Leaving edit mode
with changes asks to discard; the back button and swipe-down are disabled
while editing.

## Endpoints

All under `/api/magician/v3/tasks/{id}` unless noted:

| call | use |
|---|---|
| `GET /plan` | `plan.plan_id`, `status`, `plan_graph`, `pending_questions`; 404 = no plan |
| `PUT /plan` | `{plan, label}` |
| `GET /plan/versions?limit=20` | `versions[]`, `has_more` |
| `GET /plan/versions/{epoch_ms}` | `version.plan_graph` |
| `POST /plan/versions/{epoch_ms}/restore` | restore |
| `GET /plan/analysis` · `/plan/slots` · `/plan/attempts` · `/plan/clarifications` | panes |
| `POST /plan/clarifications/resume` | `{handled_question?, updated_confidence?, slots?}` |
| `POST /api/magician/v2/hitl/{qid}/respond` | `{source: "clarification", value: {type: "text", value}, channel: "ios", task_id}`; 409 = already answered |
| `POST /plan/approve?plan_id=` · `/plan/reject?plan_id=` · `/plan/replan` | decisions |
| `POST /execute` | Approve & Run |
| `GET /execution-panel` | live step marks |

## Opening it

`PlanInspectorRoute(taskId:title:tab:planDocument:)`. Inside a
`NavigationStack`, apply `planInspectorHost($route)` to the stack's content
and push with `@Environment(\.openPlanInspector)`, or bind
`planInspectorDestination($route)` directly; without a stack, present
`PlanInspectorSheetHost(route:)` (Close button). Review plan opens the Graph
tab, where a draft shows the approval bar.

DEBUG: `--open-plan-inspector-fixture fixture-plan-draft` (or
`fixture-plan-running`), optionally followed by `steps`, `steps-edit`,
`step-sheet`, `step-sheet-read`, `history`, `history-preview`,
`clarifications`, `slots`, `attempts` or `analysis`.

## Not ported

- Graph-canvas editing gestures (shift-drag to draw an edge, click an edge to
  delete it, ctrl-click to make a start node): on a phone these live in the
  step sheet (Depends on, Start step).
- The web legend's hover states and edge reason labels on the canvas (reasons
  show in Steps and the step sheet).
- The slot inspector's live WebSocket highlights, timeline view and slot
  detail modal (Slots is a read-only snapshot; pull to refresh).
- The tool picker (see Editing).

Tests: `MagiosTests/PlanGraphModelTests.swift`,
`PlanInspectorClientTests.swift`, `PlanInspectorModelTests.swift`.
