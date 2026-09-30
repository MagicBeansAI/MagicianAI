# Linked Task Artifacts

## Purpose

Linked task artifacts let a task consume durable outputs from earlier completed
tasks without manually restating file paths in the goal text.

## Dependency Model

The design reuses `depends_on` for durable upstream task linkage and uses
execution-scoped `linked_task_inputs` for exact artifact pinning at run start.

## Data Model

- `CreateTaskV3Request.depends_on` (`magician-api/src/task_api_v3.rs`) exposes
  upstream task links through the API.
- `TaskExecutionRecord.linked_task_inputs` pins the exact upstream artifacts
  selected for one execution.
- Durable artifacts record `source_task_id` provenance.
- Completed tasks advertise reusable outputs through
  `completion_artifact_names`.

## Authoring Flow

- In the task UI, typing `@` offers completed tasks.
- Selecting a task inserts a readable markdown mention and populates
  `depends_on`.
- Multiple upstream tasks can be linked to the same new task.

## Execution Flow

1. At task start, the orchestrator loads the linked tasks.
2. It resolves their durable artifacts and formats a read-only summary.
3. That summary is injected into execution context as linked task artifacts.
4. The planner and executor can read those files as inputs and produce new
   downstream artifacts.

The injected context is intentionally metadata first: paths, types, and short
summaries. Artifact bodies are read only when needed.

## Invariants

- Linked artifacts are read-only inputs.
- Upstream task completion still gates downstream execution.
- No automatic reruns happen when upstream artifacts change.
- Versioning and change propagation are outside the current design.
