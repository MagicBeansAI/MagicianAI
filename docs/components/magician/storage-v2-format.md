# Magician V2 Storage Layout

Live task and execution storage contract.

Provenance: Delegation V2 Ownership Transfer,
Thread-to-Task Consolidation.

## Scope

- `Task` is the durable work object; `task_id` is the durable work identity.
- `ExecutionRun` is the runtime execution object; `execution_id` is the runtime identity.
- Parent/child execution trees are stored on execution records.

`thread.json`, `thread_index.json` and `task.thread_id` are dead; new code and
tooling must not use them. Scheduler wake automation ids are fully scoped — see
[Scoped Automation Wake IDs](#scoped-automation-wake-ids).

## Directory Layout

Live state lives under the **runtime root** — `MAGICIAN_ROOT_DIR` (default
`$HOME/MagicianNotes`). If the root exists, boot resolves it once to its physical
directory, so an operator-owned symlink may point at a data volume; security-
sensitive stores still reject symlinks below the captured root. A missing root
goes through the normal create-and-validate path.

The runtime root **is** the scoped root (`ArtifactV2Workspace::resolve_scoped_root`
returns the base unchanged): scoped state is at `<runtime_root>/scopes/...`,
system state at `<runtime_root>/system/...`. `magician_data_v3` is only the repo
**seed** root; in paths below, read a leading `magician_data_v3/` as the runtime
root.

Bootstrap templates (`system/agent_templates`, `system/trust_policy_templates`,
`system/db_templates`) are read from the seed root, never copied into the runtime
root. Boot publishes a process-wide default seed root
(`workspace::set_default_seed_root`), so bare `ArtifactV2Workspace::new(...)`
resolves templates via `effective_seed_root()` (explicit `with_seed_root` →
default seed → store-root fallback), read-only. Runtime system *state*
(secrets/chat/wake) lives under `system_root()` = `<runtime_root>/system/`.

```text
<runtime_root>/   (MAGICIAN_ROOT_DIR, e.g. $HOME/MagicianNotes)
├── system/
│   └── trust_policy_templates/
│       ├── trust_policies.default.yaml
│       └── trust_policies.template.yaml
└── scopes/
    └── {principal}/
        └── {workspace}/
            ├── agent_runtime/
            │   ├── agents/
            │   │   └── {agent_id}/
            │   │       └── definition.agent.yaml
            │   ├── approvals/
            │   ├── proposals/
            │   ├── scheduler_state.json
            │   └── system/
            │       ├── trust_policies.yaml
            │       ├── trust_policies.default.yaml
            │       └── trust_policies.template.yaml
            ├── runtime/
            │   └── pause_states/
            │       └── {encoded_pause_key}.json
            ├── progress_channels/
            │   ├── subscriptions.json
            │   ├── lineage_index.json
            │   └── events/
            │       └── {log_key}.jsonl
            ├── ui/
            │   ├── chat_sessions/
            │   │   └── {session_id}/
            │   │       ├── session.json
            │   │       ├── messages/
            │   │       │   ├── 000000.jsonl
            │   │       │   └── 000001.jsonl
            │   │       └── outputs/
            │   ├── feed/
            │   │   └── feed.duckdb
            │   └── threads/
            │       ├── ui_threads.duckdb
            │       └── ui_threads.lock
            ├── executions/
            │   └── {execution_id}/
            │       ├── execution.json
            │       ├── pipeline/
            │       │   └── store.json
            │       └── artifacts/
            │           └── downloads/
            └── tasks/
                └── {task_id}/
                    ├── plans/
                    │   ├── plans_index.json
                    │   ├── plan_versions.json
                    │   └── {plan_id}.json
                    └── executions/
                        └── {execution_id}/
                            ├── execution.json
                            ├── observations/
                            ├── execution/
                            │   └── latest_summary.json
                            ├── pipeline/
                            │   └── store.json
                            └── artifacts/
                                └── downloads/
```

Notes:

- **Tasks.** `tasks/{task_id}/manifest.json`, `state/task_state.json` and
  `task_refs.json` are the live task contract (no `task_document.json`).
- **Terminal retention.** There is no per-task `terminal_retention` field (and
  no `archive_on_terminal`). Tasks carry `lifecycle`: `persistent` (stored under
  `tasks/`, listed in `/tasks`) or `internal` (stored under `internal_tasks/`,
  listed in `/internal-tasks`; `models.rs` — `TaskLifecycle`).
  - `delete_completed_task_if_safe` removes only a `completed` task with no
    active root execution, and keeps it while a published surface still needs
    the task as its source (`published_surface_requires_task_source`).
  - Chat delegations default to `internal`; `track_as_task` on
    `delegate_to_agent` makes them `persistent`. Clearing or deleting a chat
    session runs `cleanup_ephemeral_tasks_for_session`, which removes the
    terminal `internal` tasks carrying that `chat_session_id`; in-flight ones
    outlive the chat.
- **Executions.** `tasks/{task_id}/executions/{execution_id}/execution.json` is the
  canonical task-backed execution document; `executions/{execution_id}/execution.json`
  is the non-task one. No persisted `execution_index.json`; listings and recovery
  rebuild from execution documents.
- **Plans.** `tasks/{task_id}/plans/` holds PlanGraph persistence — see
  [PlanGraph Persistence](#plangraph-persistence-legacy-plans-directory-observations-and-scratch-data).
- **Pause states.** `runtime/pause_states/` is the crash-recovery root managed by `FullPauseStore`.
- **Progress channels.** `progress_channels/` holds subscriptions, lineage and event logs:
  - `subscriptions.json` and `lineage_index.json` are compact, rebuildable
    whole-map rewrites coalesced by the router flush loop. Fast lane (250 ms):
    pending-retry inserts/removals and terminal-subscription cleanup — changes a
    restart would get wrong. Slow lane (30 s): lineage cache and subscription
    watermarks; a crash loses ≤30 s of lineage (rebuilt by
    `ExecutionLineageIndex::resolve_or_load`) and leaves watermarks stale (read
    only by the listing API). Subscription create/delete write synchronously.
  - `events/{log_key}.jsonl` is only read as a bounded tail: 256 KB window,
    growing geometrically to a 4 MB ceiling. A short result is a bounded view,
    not proof the shard is short; callers needing a proven count use
    `read_jsonl_tail_adaptive_path` (fails closed). Pending-retry lookups use a
    1 MB window; older locators are treated as absent.
  - Types in `progress_channel_seam::types::EPHEMERAL_PROGRESS_EVENT_TYPES`
    (only `media.session.heartbeat`) are broadcast, never journaled, and never
    get a pending retry. Session lifecycle events stay on disk.
- **Chat sessions.** `ui/chat_sessions/{session_id}/session.json` holds metadata
  and canonical LLM transcript; display messages append to bounded JSONL
  segments under `messages/` so paginated reads avoid loading full history.
  Deleting a message rewrites only its segment. Outputs live under `outputs/`.
- **Feed.** `ui/feed/feed.duckdb` is the scope's feed read-model DB.
- **UI threads.** `ui/threads/ui_threads.duckdb` is the UI-thread DB; first use
  per process is serialized through `ui_threads.lock` until the handle is cached.
  The writable template `system/db_templates/ui_threads/schema.sql` is refreshed
  from embedded DDL when it lacks `display_mode`, `plan_mode` or `deleted_at`;
  read-only seeds are never mutated. Deleted threads are tombstoned
  (`deleted_at`), hidden from visible queries but seen by sync so old
  tasks/sessions do not recreate them. `#general` is never tombstoned.
  - `UiThreadService::sync_scope` runs on every list/page/detail/mutation (no
    timer). It learns chat-referenced threads via
    `ChatStore::list_session_thread_lanes`, which `FileChatStore` answers from
    the in-memory `search_candidates` index (unlike `list_sessions`, it also
    reports sessions whose document fails to load). The task half still calls
    `ArtifactV2Service::list_tasks` just to read `ui_thread_id` — it needs a
    task-store projection.
- **Agent runtime.** `agent_runtime/` holds live agent definitions, approvals,
  proposals and scheduler state. `agent_runtime/system/trust_policies.yaml` is
  the live scoped trust-policy file, hardened from recommended defaults on first
  scoped base-layout; sibling `.default.yaml`/`.template.yaml` are hardened copies
  of `system/trust_policy_templates/`. Top-level `system/trust_policies*.yaml`
  files are not part of the contract.
- **Observations.** `.../executions/{execution_id}/observations/` is the
  screenshot root; `.../execution/latest_summary.json` is the latest summary.

## Writing a record durably

Durable whole-file publishes live in `magician-core/src/durable_io.rs`: unique
temp (`.artifact-write-{uuid}.tmp`), `sync_all`, rename, then
`sync_parent_dir_blocking` so the rename survives power loss (rename is atomic
for ordering, not durability).

`magician/src/magician_v2/artifact_v2/io.rs` keeps the `ArtifactV2Error` wrappers
and re-exports the `io::Error` family:

| Use | Function |
| --- | --- |
| Stores that speak `ArtifactV2Error` | `write_bytes_atomic` / `write_bytes_atomic_sync` |
| Everything else | `write_bytes_durably` / `write_bytes_durably_sync` |
| Append-only writers needing only the directory sync | `sync_parent_dir_blocking` |
| A file whose permissions matter | `write_bytes_durably_with_mode` / `_sync` |
| Caller-produced staging (e.g. DuckDB `COPY TO`) | `publish_staged_file_durably_sync` |

- `_with_mode` applies the mode to the staging file *before* rename (after would
  leave a umask window); failing to apply it is an error.
- Temps are never reused, so a crash mid-write leaves uncollected residue; failed
  temp removal logs `warn!`.
- Whole-file indexes serialize load → mutate → write with a per-scope
  `tokio::sync::Mutex` (boot reconciler included). `magician-config.yaml` writers
  share `runtime_settings::config_file_lock` (taken inside
  `write_top_level_yaml_block`). One process owns a data root, so in-process
  locks suffice.
- `read_jsonl_path` fails closed on the first bad line; `read_jsonl_path_tolerant`
  keeps what parses. Tolerate for listings/existence checks; refuse for "latest X"
  or spend totals. The reader reports `torn_tail_bytes` (uncommitted tail)
  separately from `corrupt` (a committed line lost).
- Adoption is ratcheted by `make check-store-durability`. History:
  store durability adoption.

## Execution Document

`tasks/{task_id}/executions/{execution_id}/execution.json` holds
`ExecutionRunDocument`: `execution` (`ExecutionRun`), `turns`, `slots`, `states`,
`clarification_session`, `clarification_history`.

High-signal `ExecutionRun` fields: `id`, `principal`, `workspace`, `task_id`,
`root_execution_id`, `title`, `waiting_state`, `processing_correlation_id`,
`current_stage`, `current_provider`, `escalation_trigger`, `parent_execution_id`,
`child_execution_ids`, `active_owner_agent_id`, `owner_stack`,
`active_delegation_group`, `timeout_secs`, `delegation_chain`, `paused_from_state`.

The execution tree lives on the records: root `parent_execution_id = null`,
children point to their parent, parents list `child_execution_ids`. No separate
tree document.

## Execution Index

There is no persisted `runtime_v2/execution_index.json`. Execution documents are
the source of truth; listing and recovery rebuild from them.

## I/O Resilience

File-descriptor exhaustion (`ENFILE`/`EMFILE`, OS errors 23/24) is transient on
V3 filesystem hot paths: JSON/JSONL reads, atomic writes, event appends, task
lock acquisition, journal recovery, task reads/deletion and preserved-output
copies retry briefly with bounded backoff, then return the original error. Other
errors (missing file, permissions, bad JSON) fail normally.

## Task Records

```text
tasks/{task_id}/manifest.json          # TaskManifest: durable metadata
tasks/{task_id}/state/task_state.json  # TaskState: lifecycle + root-execution pointers
tasks/{task_id}/task_refs.json         # TaskRefs: output refs, surface pointers
tasks/{task_id}/executions/{execution_id}/...  # root-execution history
```

High-signal `TaskManifest` / `TaskState` fields: `id`, `principal`, `workspace`,
`title`, `description`, `status`, `priority`, `tags`, `agent_id`, `schedule`,
`created_by`, `depends_on`, `approved`, `has_plan`, `active_root_execution_id`,
`latest_root_execution_id`, `last_completed_root_execution_id`, `error_message`,
`retry_at`, `current_step`, `progress`, `completion_summary`,
`completion_outcome`, `completion_artifact_names`, `last_progress_at`.

### `last_progress_at` is not `updated_at`

`TaskState.last_progress_at` (RFC3339, absent until the task runs) records when
the run last **actually advanced**; it is mirrored onto `TaskListItemV3` so
surfaces can report a wedged run. `updated_at` cannot: the realtime bridge
drives `reduce_runtime_signal` from every mirrored event, so a wedged run
rewrites it continuously.

Exactly four sites may write it:

| site | why it counts |
|---|---|
| `reduce_execution_initialized` | first step starts (`StepStarted{step_execute_task}`) |
| `activate_execution` | a re-activated run's step starts |
| `reduce_step_event` (`StepCompleted` / `StepFailed`) | a step finished; a failed step still advanced the plan |
| `reduce_action_settled` (`ToolSucceeded` / `ToolFailed`) | a tool call settled — the flat loop's unit of advancement; direct runs have no taskplan steps and would otherwise read as stalled |

All other writes (status re-asserts, metric flushes, metadata edits, synthesis
bookkeeping, monitor cursors) leave it alone; `reduce_step_event` does not touch
`updated_at`. Absence is an omitted key, never `0`/`-1`; it is a timestamp, not a
`stalled: bool` — clients compute `now - last_progress_at` and own the threshold.
Tests: `artifact_v2/reducer.rs`, `artifact_v2/service.rs` (`last_progress_at_*`,
`task_list_item_*`).

Invariants:

- Task records do not store `thread_id` or a generic live `execution_id` alias.
- Root execution pointers live on the task; child executions are discovered from
  the execution tree.
- No `TaskDocument` wrapper and no `task_document.json` sidecar.

## Scoped Automation Wake IDs

Automation wakes use deterministic scoped task ids:

- format: `scoped:<hex(principal)>:<hex(workspace)>:<hex(agent_id)>:<hex(goal_id)>`
- producer: `scoped_automation_task_id(principal, workspace, agent_id, goal_id)`
- consumer: runtime decodes the hex payload before resuming the automation entry

Why: deterministic routing without ambiguous unscoped `agent_id:goal_id`
parsing. `AgentScheduler` requires scoped storage; there is no in-memory-only mode.

## Execution History

Each `TaskExecutionRecord` is one root run: `execution_id`, `started_at`,
`ended_at`, `status`, `error_message`, `completion_summary`,
`completion_outcome`, `completion_artifact_names`, `current_step`, `progress`,
`plan_id`, `artifact_chain_id`, `linked_task_inputs`, `step_statuses`. Delegated
child structure lives in execution storage, not in these rows.

## PlanGraph Persistence (legacy `plans/` directory), Observations, and Scratch Data

`tasks/{task_id}/plans/` and the `TaskPlan*` types (`TaskPlanRecord`,
`TaskPlanIndexRecord`, `TaskPlanVersionsRecord`, `TaskPlanStatus`, …) are
misnamed: they are per-task persistence for a `PlanGraph`.

- `plans_index.json` — latest + approved `PlanGraph` ids
- `{plan_id}.json` — versioned envelope: `PlanGraph` plus planning artifacts
  (`query_analysis`, `slot_graph_snapshot`, `strategy_attempts`,
  `clarification_history`, `pending_questions`, `status`)
- `plan_versions.json` — envelope version history

`plans/` exists only for tasks that went through Plan mode
(`process_with_strategy`: `/tasks` Plan button, Replan, chat
`ChatMessageMode::Plan`). Do-mode tasks (`delegate_to_agent`,
`handover_to_agent`, `spawn_sub_goal`, `doit_direct`) have none, so
`GET /tasks/{id}/plan` returns `task_plan_not_found:{id}`; the UI guards on
`task.hasPlan`.

The markdown task plan (`taskplan_live.md`, `projected_plan_summary_*.md`) is
retired; old envelopes' `taskplan_markdown` field is dropped by serde. See
inner-loop runtime-context compaction.

`executions/{execution_id}/` owns observations and `latest_summary.json`. Key
run-specific state by `execution_id`, never by task id alone or thread id.

## Pause State Persistence

Pause state for crash recovery is at
`scopes/{principal}/{workspace}/runtime/pause_states/{encoded_pause_key}.json`,
execution-keyed and scoped; exact resume uses the persisted `execution_id`.
Agent-scoped pause state uses the `agent:` key namespace.

## Deletion Semantics

Deleting an execution removes
`tasks/{task_id}/executions/{execution_id}/` — `execution.json`, observations and
scratch/version history. Deleting a task removes the task directory; runtime
execution cleanup still follows the execution store contract.

### The task flock is not reentrant

For a task-bound execution, `FileV2Store`'s write guard is a per-task lock:
`lock_path_for_execution` resolves to `<scope>/.task_lifecycle/locks/<task_id>.lock`.
`flock` is per open-file-description, so a second `open()` of that path — even
on the same thread — blocks forever.

The task flock is also the cross-process fence for runtime cancellation and
pipeline stage commits (`acquire_pipeline_task_transaction_guard`). A holder that
calls an ordinary store write deadlocks. So a fence holder must either:

- use `compare_exchange_execution_status_holding_task_lock`, which routes through
  `with_execution_document_under_held_task_lock` (exclusion holds: same lock, plus
  the per-execution async mutex in-process), or
- drop the fence first when the write need not be linearized against it.

`ArtifactV2Workspace::task_start_admission_lock_path` is a distinct file for the
same reason: the reducers it guards take the ordinary task-record lock.

## Guidance For New Code

Do: read/write execution state via `ExecutionRunDocument` and task state via
`TaskManifest`/`TaskState`/`TaskRefs`; use `execution_id` for runtime identity
and `task_id` for durable identity; traverse trees via `parent_execution_id` /
`child_execution_ids`.

Don't: reintroduce `thread_id` or thread-shaped storage, or dual-write into
thread-shaped files.
