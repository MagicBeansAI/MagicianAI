# Execution Native Panels

The execution, preplanning, slot, and deep-operator surfaces use native Svelte
controls instead of GAUI/MUIJ primitives. `ExecutionPanel.svelte` is gone;
the task/run drawer is `UnifiedTaskPanel.svelte` inside `TaskPanelDrawer.svelte`.

## Scope

These surfaces import the shared native primitive set from
`$lib/magician/components/native/*`:

- `src/lib/magician/tasks/UnifiedTaskPanel.svelte`
- `src/lib/magician/tasks/TaskPanelDrawer.svelte` (header hysteresis in
  `taskPanelHeader.ts`)
- `src/routes/(app)/ExecutionPlanInspector.svelte`
- `src/routes/(app)/EventLogView.svelte`
- `src/routes/(app)/PlanGraphView.svelte`
- `src/routes/(app)/SlotGraphInspector.svelte`
- `src/routes/(app)/SlotTimelineView.svelte`
- `$lib/magician/components/ExecutionResponsibilityPanel.svelte`
- `$lib/magician/components/ExecutionTimeline.svelte`
- `$lib/magician/components/execution/ExecutionControls.svelte`

`PlanningActivityStream.svelte`, `$lib/realtime/EventStreamCard.svelte`, and
`$lib/hitl/*` share planning/HITL behaviour with these surfaces but do not
import the native set.

The native set is Badge, Button, Card, Checkbox, EmptyState, Select, Skeleton,
Spinner, Stack, Table, Tabs, Tag, Text, Tooltip, and TreeSelect.

## Theming Rules

- Use `native-*` classes for component internals.
- Use theme tokens for color, spacing, radius, borders, and shadows.
- Do not introduce new `.muij-*` selectors in execution panel code.
- Do not use direct hex/RGBA colors or old-palette token fallbacks in migrated
  execution surfaces. D3/SVG rendering should resolve colors from active theme
  tokens before drawing.
- Keep retro theme overrides local to the native primitive when the primitive
  needs different geometry.

## Task-panel header

`TaskPanelDrawer` owns the chrome; `UnifiedTaskPanel` owns the Plan / Run /
Output acts (`id` `plan` / `run` / `output`).

The sticky header uses hysteresis rather than a single scroll cutoff:
`HEADER_CONDENSE_AT_PX = 72` and `HEADER_EXPAND_AT_PX = 12` in
`taskPanelHeader.ts`. The header is a sibling of the scrolling body, so
changing its height cannot alter the `scrollTop` that drives compaction.
Condensing hides the description (up to five lines, click-expandable);
the title clamps from two lines to one. The thread control is a **Move to**
native `Select` on its own row — do not restamp the selected thread as a
title-row badge. Status and plan chips come from `headerChips()` (status
word plus optional `Plan: …`) immediately above the acts.

## Planning And HITL Behavior

- **Plan** starts task-scoped planning; it does not execute the task. Pending
  planner questions submit through the canonical `/v2/hitl/{id}/respond` route.
- Chat and execution chrome share one composer mode: **Do** (omit `mode`),
  **Accept** (`mode: accept_in_scope` — in-tree file edits skip permission
  HITL, including `apply_patch` hunks; `diff_approval` still parks unless
  Autopilot can self-apply; destructive confirmations and out-of-tree paths
  still prompt), and
  **Plan** (`mode: plan` — reads and ask, no mutate). Cycle it on the composer,
  not a settings page. Magios and Magdroid post the same field.
- Attention, chat, and the Plan act share the same canonical HITL request and
  response types. Tool authorization and sandbox override requests use the
  choice surface with `allow_once` and `deny` decisions. `input_type=form` is
  stacked fields with Skip / Skip all. Chat hosts that cannot render a form
  open Attention instead of flattening to a text box. The desktop notification
  overlay still only **opens** that prompt; it does not resolve it.
- A single waiting item opens the global prompt through `openHitlPrompt`; a
  multi-item CTA opens the paginated Attention list.
- Planning and run activity streams use long-lived requests and reconnect after
  ordinary response-body closure or abort. A replaced stream must not render
  browser transport messages such as `BodyStreamBuffer was aborted` as task
  errors (`PlanningActivityStream.svelte`, `EventStreamCard.svelte`).
- Terminal task failures are inspected in task/run/history surfaces. Attention
  is reserved for unresolved operator decisions and does not list task-failure
  notifications.

## Data Lifecycle

`executionPanelStream.ts` is the live subscription for chat *Inspect run* and
`/crew/<id>` cycles. An `ExecutionPanelDelta` is a complete snapshot, not a
patch: the newest matching state in a batch replaces the model. Identity is
the execution id read off the state (`executionIdOf`), scoped by principal and
workspace — not the synthetic `agent-cycle:` task id and not a task-id match
that would mix two runs of the same task.
