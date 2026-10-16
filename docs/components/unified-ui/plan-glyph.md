# Plan glyph

A small "account tree" mark that makes a planned task recognisable at a glance.
Web: `ui/unified-ui/src/lib/magician/tasks/PlanGlyph.svelte`; iOS uses the SF
Symbol equivalent and Android Material `AccountTree`, under the same rule.

## The rule

`taskHasPlan(task)` (`ui/unified-ui/src/lib/magician/tasks/taskHasPlan.ts`) is
true when any of:

- `task.hasPlan === true`
- `task.planStatus` is set (any value, including `rejected` / `failed`)
- `task.latestPlanId` or `task.approvedPlanId` is set
- `task.planSteps` has at least one step

The chat live task card uses `liveTaskHasPlan({ messageStatus, storeStatus,
storePlan, planSteps })`, which is also true when the card message's or the
task store's status is `planning`, `draft` or `eliciting`, because the card
often has only a status message before the store record carries plan fields.

A quick chat task with no plan shows no glyph.

## Rendering

- Inline SVG (Material `account_tree` path), `fill: currentColor`, colour
  `var(--accent-secondary)`.
- `size` prop, default 13px; the wrapper is `inline-flex`, sized to `size`,
  `flex: none`, `vertical-align: middle`, so it never shifts the title.
- `role="img"`, `aria-label` and `title` both "Planned task" (overridable via
  the `title` prop). Static: no animation.
- The status dot is unchanged; the glyph is an addition, not a replacement.

## Placement contract

Directly before the title text, inside the title's row, in:

1. the chat live task card title row (`chat/next/LiveTaskCard.svelte`,
   `.cn-live-task__top`, before `.cn-live-task__title`);
2. the Tasks list row (`tasks/NativeTasksSurface.svelte`, after the status dot
   and before `.native-task-title-button`);
3. the task drawer header (`tasks/TaskPanelDrawer.svelte`, inside
   `h2.task-panel__title-row`, before `.task-panel__title`).
