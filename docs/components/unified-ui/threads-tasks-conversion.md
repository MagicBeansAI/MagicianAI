# Task And Execution UI Architecture

The UI does not convert backend runtime threads into frontend tasks. Background:

- Delegation V2 Ownership Transfer Design
- Thread-to-Task Consolidation Design

## Current Model

- `Task` is the primary work object shown in task surfaces; `task_id`
  identifies durable work.
- `ExecutionRun` is the runtime object shown in execution surfaces;
  `execution_id` identifies one root or child execution run.

`taskStore.ts` reads and writes tasks only through `/api/magician/v3/tasks`
(its other calls are `/api/magician/v3/executions/{id}/cancel` and VibeDev run
lists). The only V2 task route is `GET`/`POST /api/magician/v2/tasks/{task_id}/budget`.

## UI Responsibilities

### Task surfaces

Task lists, summaries, counts and metadata editors use the V3 task plane:

- `GET /api/magician/v3/tasks`: list. Lane membership is
  `?view=all|inbox|today|overdue|running|completed`. Counts are **inline** on
  that envelope (`body.counts`); there is no `/tasks/counts` route.
- `POST /api/magician/v3/tasks`: create
- `GET /api/magician/v3/tasks/{id}`
- `PUT /api/magician/v3/tasks/{id}`
- `DELETE /api/magician/v3/tasks/{id}`
- `POST /api/magician/v3/tasks/{id}/plan` and `GET …/plan`
- `POST /api/magician/v3/tasks/{id}/execute`

There is no `POST /tasks/{id}/pause` or `/stop` in any version. Pause, resume,
steer and abort are execution-scoped.

Task rows render title, description, status, assigned agent,
approval/dependency state, root execution pointers and progress. Dense rows
keep `ui_thread_id` / channel labels on a reserved first line.

### Execution surfaces

Execution panels, responsibility views, logs, runtime clarification and
pause/resume use execution APIs and events. Task-plan reads stay task-scoped on
V3:

- `GET /api/magician/v2/executions/{id}`
- `GET /api/magician/v2/executions/{id}/responsibility`
- `POST /api/magician/v2/executions/{id}/clarify/{question_id}/respond`
- `POST /api/magician/v2/executions/{id}/pause`
- `POST /api/magician/v2/executions/{id}/resume`
- `POST /api/magician/v2/executions/{id}/abort`
- `GET /api/magician/v3/tasks/{id}/plan`
- `GET /api/magician/v3/tasks/{id}/execution-panel?execution_id=<execution_id>`

They render waiting state, owner, owner stack, active children, and
execution-scoped logs, plans and observations.

## Data Shapes

### Task

Key fields: `id`, `status`, `agent_id`, `approved`, `depends_on`, `has_plan`,
`active_root_execution_id`, `latest_root_execution_id`,
`last_completed_root_execution_id`.

There is no `task.thread_id`, no `task.execution_id` as a generic live alias,
no `task.execution_history` (retention lives on the schedule as
`execution_history_retention`), and no thread reuse across runs.

### Execution

Key fields: `id`, `task_id`, `root_execution_id`, `parent_execution_id`,
`child_execution_ids` / `active_delegation_group`, `waiting_state`,
`active_owner_agent_id`, `owner_stack`, `paused_from_state`.

Tree hydration starts from the task's root execution pointer, then follows
parent/child links.

## Status Semantics

Task status answers "what is the durable work state?"; execution waiting state
answers "what is this run doing now?". Examples: task `running` + execution
`waiting_user`; task `running` + execution `waiting_children`; task `ready` +
no active root execution; task `completed` + last completed root execution
available to inspect. The UI never collapses these into one thread status.

## Planning And Execution

Planning is part of the root execution lifecycle:

1. create or resolve the task
2. prepare or reuse the root execution
3. run planning and execution against that execution id
4. surface task and execution updates together

Direct execution entrypoints still produce task-backed root executions. There
is no separate `/threads` create-or-convert flow.

## Delegation V2

Responsibility is execution-scoped: handover changes the active owner on the
same execution; parallel delegation creates child executions under the same
task; responsibility panels show the owner chain and active children. So the
execution panel is keyed by `execution_id`, task lists by `task_id`.

## Realtime Contract

Reconnect and filter execution events by `execution_id`; join task updates by
`task_id` on task surfaces. The UI does not depend on `/threads/...` routes,
synthetic cycle thread aliases, or generic `thread_id` fields in live payloads.

## Guidance For UI Work

Fetch tasks for task surfaces and executions for execution surfaces; treat
execution panels as execution-scoped overlays; render responsibility with
`ExecutionResponsibilityPanel.svelte` and panel data with
`executionPanelModel.ts` / `executionPanelStream.ts`. Do not add thread-first
stores or view models, rebuild task data from execution summaries, reintroduce
`/threads/...` fetches, or treat `task_id` and `execution_id` as
interchangeable.
