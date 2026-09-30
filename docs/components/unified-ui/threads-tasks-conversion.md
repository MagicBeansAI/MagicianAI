# Task And Execution UI Architecture

The UI does not convert backend runtime threads into frontend tasks. Background:

- Delegation V2 Ownership Transfer Design
- Thread-to-Task Consolidation Design

## Current Model

- `Task` is the primary work object shown in task surfaces.
- `ExecutionRun` is the runtime object shown in execution surfaces.
- `task_id` identifies durable work.
- `execution_id` identifies a specific root or child execution run.

Think in task/execution terms, not thread/task conversion.
`taskStore.ts` talks to `/api/magician/v3/tasks` exclusively; there is no `/api/magician/v2/tasks` scope.

## UI Responsibilities

### Task surfaces

Task lists, summaries, counts, and metadata editors use the V3 task plane:

- `GET /api/magician/v3/tasks` — list. Lane membership is
  `?view=all|inbox|today|overdue|running|completed`. Counts are **inline** on
  that envelope (`body.counts`); there is no `/tasks/counts` route.
- `POST /api/magician/v3/tasks` — create
- `GET /api/magician/v3/tasks/{id}`
- `PUT /api/magician/v3/tasks/{id}`
- `DELETE /api/magician/v3/tasks/{id}`
- `POST /api/magician/v3/tasks/{id}/plan` and `GET …/plan`
- `POST /api/magician/v3/tasks/{id}/execute`

There is no `POST /tasks/{id}/pause` or `/stop` in any version. Pause, resume,
steer, and abort are execution-scoped (below).

Task rows should render title, description, status, assigned agent,
approval/dependency state, root execution pointers, and completion/progress.
Dense rows keep `ui_thread_id` / channel labels on a reserved first line.

### Execution surfaces

Execution panels, responsibility views, logs, runtime clarification, and
pause/resume use execution APIs and execution events. Task-plan reads stay
task-scoped on V3:

- `GET /api/magician/v2/executions/{id}`
- `GET /api/magician/v2/executions/{id}/responsibility`
- `POST /api/magician/v2/executions/{id}/clarify/{question_id}/respond`
- `POST /api/magician/v2/executions/{id}/pause`
- `POST /api/magician/v2/executions/{id}/resume`
- `POST /api/magician/v2/executions/{id}/abort`
- `GET /api/magician/v3/tasks/{id}/plan`
- `GET /api/magician/v3/tasks/{id}/execution-panel?execution_id=<execution_id>`

Render waiting state, owner, owner stack, active children, and execution-scoped logs, plans, and observations.

## Data Shapes

### Task

Important task fields: `id`, `status`, `agent_id`, `approved`, `depends_on`,
`has_plan`, `active_root_execution_id`, `latest_root_execution_id`,
`last_completed_root_execution_id`.

The UI should not expect `task.thread_id`, `task.execution_id` as a generic live
alias, `task.execution_history` (retention lives on the schedule as
`execution_history_retention`), or thread reuse semantics across runs.

### Execution

Important execution fields: `id`, `task_id`, `root_execution_id`,
`parent_execution_id`, `child_execution_ids` / `active_delegation_group`,
`waiting_state`, `active_owner_agent_id`, `owner_stack`, `paused_from_state`.

Execution tree hydration starts from the root execution pointer on the task,
then follows parent/child execution links.

## Status Semantics

Task status and execution waiting state are related but not identical:

- task status answers: "what is the durable work state?"
- execution waiting state answers: "what is this run doing right now?"

Examples: task `running` + execution `waiting_user`; task `running` + execution
`waiting_children`; task `ready` + no active root execution; task `completed` +
last completed root execution available for inspection.

The UI should not collapse these into one generic thread status.

## Planning And Execution

Planning is part of root execution lifecycle, not a hidden thread bootstrap step.

1. create or resolve task
2. prepare or reuse root execution
3. run planning/execution against that execution id
4. surface task updates plus execution updates together

Direct execution entrypoints still produce task-backed root executions. The UI
should not rely on a separate `/threads` create-or-convert flow.

## Delegation V2

Delegation V2 responsibility surfaces are execution-scoped:

- handover changes the active owner on the same execution
- parallel delegation creates child executions under the same task
- responsibility panels should show owner chain plus active child executions

This is why the execution panel is keyed by `execution_id`, while task lists
remain keyed by `task_id`.

## Realtime Contract

Realtime filters and reconnect are execution-aware: reconnect by `execution_id`,
filter execution events by `execution_id`, and join task-aware updates using
`task_id` when rendering task surfaces.

The UI should not depend on `/threads/...` routes, synthetic cycle thread
aliases as primary runtime ids, or generic `thread_id` envelope fields in live
payloads.

## Guidance For UI Work

New UI work should fetch tasks for task surfaces, fetch executions for
execution surfaces, treat execution panels as execution-scoped overlays, and
use GAUI/shared components for responsibility and execution rendering where
possible.

New UI work must not add thread-first stores or view models, recreate task
data by converting execution summaries, reintroduce `/threads/...` fetches, or
assume `task_id` and `execution_id` are interchangeable.
