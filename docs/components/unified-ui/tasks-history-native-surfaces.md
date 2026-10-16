# Tasks And History Native Surfaces

The create-task form is a native Svelte surface (`TaskCreateForm.svelte`, shared
task types) rather than the old Spells MUIJ form subtree, and keeps
assignment, thread selection, output mode, schedule fields and task references.
Legacy `/tasks` and `/t/[name]/tasks` links normalize to `/work`
(`src/lib/work/routes.ts`).

`TasksWorkspace.svelte` is the shared controller for route-backed Tasks and
local hosts such as Town Square. Its mounted regression contract follows a
persistent task through pending, planning, ready, running, paused,
waiting-for-user, resumed and completed projections from the real task store
and scoped `TaskUpdated` bridge. Separate cases cover operator stop,
cancelled/failed reset, recurring executions returning to Ready, and confirmed
deletion. The cross-surface Attention case answers a waiting task through the
canonical HITL endpoint and requires the next realtime projection to render
Running; the fixture never mutates component state directly.

## How people reach the task list

The task list lives on the **Work** top-bar tab (`/work`; Chat · Work ·
Observe · Today). Chat's live task card opens the task's progress panel (see
`chat-next.md`). The command palette's **Work** entry (`G T`, which the
`(app)` layout also binds as a leader shortcut) under *Jump to* covers tasks, crew, monitors, internal work, programs and
quality review. `TopBar.component.test.ts` holds the nav contract; `CommandPalette.component.test.ts` holds the palette entries.

## Filter lanes come from the server

The six lanes (`all`, `inbox`, `today`, `overdue`, `running`, `completed`) are
answered by `GET /api/magician/v3/tasks?view=<lane>`, with predicates in
`magician/src/magician_v2/task_lanes.rs`. `taskStore` asks for the active lane
on load and on every filter change and tags the answer with its lane, so a
stale answer is never applied to a lane the reader has left.

- `view=` and `counts` live on the endpoint's **paged** branch only, so the
  request carries `limit`/`offset`, with a 100,000-row page
  (`TASK_LANE_PAGE_LIMIT`) so one page holds the corpus.
- It is a **second** request beside the one that fills `taskStore.tasks`. That
  array is the app's task corpus (mention picker, command palette, thread task
  list, vibe cockpit), so it is never narrowed to a lane. Only lane
  *membership* comes from the server; rows stay the store's own records, so an
  optimistic rename or tick shows at once.
- `today=YYYY-MM-DD` is the **reader's** local date from `readerLocalDate()`.
  The server refuses to guess it for the two date lanes (400
  `task_view_requires_today`).

`matchesTaskLane()` mirrors the predicates on the client and is used **only
where there is no server answer**: the warroom's in-flight lanes, the
thread-scoped task list, and the list during a filter switch's round trip.

### The grace period stays local

`pendingCompletions` holds tasks the reader just ticked. They stay in the lane
they were ticked in and out of Completed until the grace period ends. The
server answers from stored state and must not model this, or a ticked task
would vanish at once.

`pendingCompletionLaneDelta()` is the single definition of that adjustment.
`applyPendingCompletionOverlay()` applies it to the rows and
`laneCountWithPendingCompletions()` to the badge, so count and list cannot
drift apart.

### Badges count the corpus

`taskCounts` reports the endpoint's `counts` object (beside `pagination`): each
lane's total over the whole scoped pool. The active lane's badge moves with the
same overlay as its rows; the other five are shown as counted.

A lane the server did not report renders **no badge, never `0`**. The server
omits `counts` when `today` is absent and on the legacy unpaged branch.

`/history` remains a native inspector route for persisted agent episodes. Its
chrome, episode list, pagination and detail drawer should use theme-derived
surfaces from the route CSS, not plain white or `--bg-subtle` fallback boxes.
