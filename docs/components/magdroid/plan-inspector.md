# Plan Inspector (Android)

The Android port of the iOS Plan Inspector
([magios/plan-inspector.md](../magios/plan-inspector.md)); approved design:
§7 of
`docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.
Views, editing rules and endpoints are the iOS ones; this page lists only the
Android files, the entry API and what differs.

| file | role |
|---|---|
| `bridge/…/tasks/PlanGraph.kt` | graph model, levels, waterfall, layered layout, cycle-safe edits, lossless save payload |
| `bridge/…/tasks/PlanInspectorRequests.kt` | `PlanInspectorClient` request builders and calls, `PlanInspectorTransport` |
| `bridge/…/tasks/PlanInspectorRules.kt` | `PlanInspectorRoute`, `PlanInspectorState` (load, save, history, panes, clarifications, approval, live marks) |
| `bridge/…/tasks/HttpPlanInspectorTransport.kt` | the transport over Ktor with the `MagicianAccess` host, Access pair and bridge token |
| `app/…/ui/PlanInspectorScreen.kt` | entry (`PlanInspectorHost`, `LocalOpenPlanInspector`, `PlanInspectorDialog`), `PlanInspectorController`, top bar, header chips, Graph · Steps · History segments, banners, approval bar, confirmations |
| `app/…/ui/PlanGraphCanvas.kt` | the zoomable DAG (Compose `Canvas` edges + node composables), zoom −/+ / Fit, legend |
| `app/…/ui/PlanStepsList.kt` | waterfall cards, the edit list (drag handle, swipe / ✕ delete, Add step), shared tags, cards, JSON disclosure |
| `app/…/ui/PlanStepSheet.kt` | one step in a `ModalBottomSheet`: read-only fields, or editors in edit mode |
| `app/…/ui/PlanInspectorPanes.kt` | History (versions → preview → Restore), Analysis, Slots, Attempts, Clarifications, plan document |
| `app/…/ui/PlanInspectorFixtures.kt` | DEBUG canned plans (`fixture-plan-draft`, `fixture-plan-running`) |

## Opening it

```kotlin
// Once, around the screen tree (inside MagicanTheme):
PlanInspectorHost { /* app content */ }

// Anywhere below it:
val openPlan = LocalOpenPlanInspector.current
openPlan?.invoke(PlanInspectorRoute(taskId, title = task.title, tab = PlanInspectorTab.Graph, planDocument = markdown))
```

Without a host, hold a `PlanInspectorRoute?` and show
`PlanInspectorDialog(route, onClose = { route = null })`. The inspector is a
full-screen dialog window, so it covers the bars wherever the caller sits.

## Android specifics

- `PlanInspectorState` is plain Kotlin; `PlanInspectorController` runs its
  suspend actions on the main dispatcher and bumps a Compose revision when
  each starts and ends, so in-flight flags (Saving…, Approving…) show at once.
- Live marks poll `/execution-panel` every 4 s only while the run is active and
  the screen is `RESUMED` (`repeatOnLifecycle`).
- Back: closes a pane or the plan document, then asks before discarding
  unsaved edits, then leaves a history preview, then closes.
- Reorder is a long press on a row's handle, then drag; delete is swipe left
  or ✕ (draft only, nothing is saved until Save).
- The dialog window clears `FLAG_DIM_BEHIND`: with it, the system draws light
  bar icons over the inspector's light top bar.
- Motion (JSON disclosure) is off when the animator duration scale is 0.

DEBUG: on a debug build under a host,
`adb shell setprop debug.magdroid.plan_fixture "fixture-plan-draft steps"`
opens a fixture when the host is next composed (optional view: `graph`,
`steps`, `steps-edit`, `step-sheet`, `step-sheet-read`, `history`,
`history-preview`, `clarifications`, `slots`, `attempts`, `analysis`); clear
it with `setprop debug.magdroid.plan_fixture ""`. Fixture actions never reach
the network.

## Not ported

As iOS: canvas editing gestures (dependencies and Start live in the step
sheet), edge reason labels on the canvas, the slot inspector's live
highlights, and a tool picker (the tool is free text).
