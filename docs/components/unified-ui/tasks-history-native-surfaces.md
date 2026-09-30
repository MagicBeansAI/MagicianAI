# Tasks And History Native Surfaces

The stable `/tasks` and `/t/[name]/tasks` create-task forms are native Svelte
surfaces. They use `TaskCreateForm.svelte` and shared task types instead of
embedding the old Spells MUIJ form subtree, while preserving assignment,
thread selection, output mode, schedule fields, and task references.

`TasksWorkspace.svelte` is the shared controller for route-backed Tasks and
local hosts such as Town Square. Its mounted regression contract follows a
persistent task through pending, planning, ready, running, paused,
waiting-for-user, resumed, and completed projections delivered by the real task
store and scoped `TaskUpdated` bridge. Separate cases protect operator stop,
cancelled/failed reset, recurring executions returning to Ready, and confirmed
deletion. The cross-surface Attention case answers a waiting task through the
canonical HITL endpoint and requires the subsequent realtime task projection to
render Running; the fixture does not mutate component state directly.

## Filter lanes come from the server

The six filter lanes — `all`, `inbox`, `today`, `overdue`, `running`,
`completed` — are answered by `GET /api/magician/v3/tasks?view=<lane>`, whose
predicates live in `magician/src/magician_v2/task_lanes.rs`. `taskStore`
asks for the active lane on load and on every filter change, and keeps the
answer as a set of ids tagged with the lane it answers, so a stale answer can
never be applied to a lane the reader has since moved to.

Three things about the request:

- `view=` and `counts` live on the endpoint's **paged** branch only. The request
  therefore carries `limit`/`offset`; the page is deliberately large enough to
  hold the corpus, because Phase 0 moves who computes the lane without changing
  how much is loaded.
- It is a **second** request, beside the one that fills `taskStore.tasks`. That
  array is the app's task corpus — the mention picker, the command palette, the
  thread task list and the vibe cockpit all read it whole — so narrowing it to
  `/tasks`' current lane would silently shrink every one of them. Only lane
  *membership* comes from the server; the rows themselves stay the store's own
  records, so an optimistic rename or tick still shows immediately.
- `today=YYYY-MM-DD` is the **reader's** local date, from `readerLocalDate()`.
  The server refuses to guess it for the two date lanes (400
  `task_view_requires_today`), and it is the one derivation the lane predicates,
  the grace-period overlay and the request all share.

`matchesTaskLane()` mirrors those predicates on the client and is used **only
where there is no server answer**: the warroom's in-flight lanes and the
thread-scoped task list derive over pools the endpoint cannot describe, and the
`/tasks` list needs something to render across the round trip a filter switch
costs.

### The grace period stays local

`pendingCompletions` holds tasks the reader just ticked. They linger in the lane
they were ticked in rather than vanishing under the cursor, and are held out of
Completed until the grace period ends. The server answers a lane from stored
state and must not model this — moving it server-side would make a ticked task
disappear instantly, which is what the grace period exists to prevent.

`pendingCompletionLaneDelta()` is the single definition of that adjustment.
`applyPendingCompletionOverlay()` applies it to the rows and
`laneCountWithPendingCompletions()` applies its sizes to the badge, so the number
above the list and the list beneath it cannot drift apart.

### Badges count the corpus

`taskCounts` reports the `counts` object the endpoint returns beside
`pagination` — every lane's total over the whole scoped pool, counted before any
lane filter. The active lane's badge is moved by the same overlay that moves its
rows; the other five are reported as counted, since their rows are not on screen.

A lane the server did not report renders **no badge, never `0`**. The server
omits `counts` entirely when `today` is absent and on the legacy unpaged branch,
so absence is a real state, and a fabricated zero would be a claim nobody made.

`/history` remains a native inspector route for persisted agent episodes. Its
page chrome, episode list, pagination controls, and detail drawer should use
theme-derived surfaces from the route CSS rather than plain white or
`--bg-subtle` fallback boxes.
