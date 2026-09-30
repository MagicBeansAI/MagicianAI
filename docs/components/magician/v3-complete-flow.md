# Magician V3 Complete Flow (Unified Agentic Architecture)

Current-state map of V3 boot, config, storage, planning, and the shared
direct-agentic runtime. Source of truth is the code, not this file.

- Magician crate: `0.7.13`
- Binary: `magician-bin/src/main.rs` (HTTP default `:3002`)
- API handlers: `magician-api/src/`
- Pre-execution-run flow (archived):
  `docs/archive/magician/v2-complete-flow.md`
- Concept model: [`execution/UNIFIED_AGENTIC_ARCHITECTURE.md`](execution/UNIFIED_AGENTIC_ARCHITECTURE.md)
  (workflows are out of runtime scope there; this runtime does not implement
  a YAML workflow engine, `/workflows` routes, or `workflowStore.ts`)

Delegation V2 and the execution-run rewrite use `execution_id`, `ExecutionRun`,
and execution-tree semantics. User-facing chat threads are a separate surface.

This document covers:

- **Planning path.** `PlanningOrchestrator::run()` produces a `PlanGraph` and
  stops. Non-task intents terminate during planning. Task intents flow through
  intent classification → slot extraction → elicitation → query rewriting →
  planning (and optional weak-step refinement).
- **Execution path.** Supported execution converges on
  `MagicianV2Orchestrator::execute_agentic_direct_with_outcome()`. A stored or
  approved `PlanGraph` is rendered into advisory runtime context. If no graph
  exists, execution starts from the task goal and bounded runtime context.
- **Plan editing.** Plan review/edit surfaces edit `PlanGraph` directly. The
  runtime does not import, export, or regenerate taskplan markdown from graph
  edits, planning refreshes, approval, or execution handoff.
- **Task-plan lifecycle.** Only `draft` plans are approvable. `planning` and
  `eliciting` plans stay non-executable. Replans create a new latest plan
  record. Stale background planning refreshes and failure writes are ignored
  once a plan is terminal (`approved`, `rejected`, or `failed`).
- **Draft visibility.** `draft` plans stay in task `pending` until explicit
  approval. UI surfaces route them to plan review, not runnable `ready` work.
- **Restart model.** The execution event log, pause state, artifacts, and
  runtime-context state are the durable execution ledger. The same
  `execution_id` can resume its own artifact lineage; a new execution on the
  same task starts fresh. On process restart, startup reconciles stale
  `Running`/`Planning` task records and rehydrates deferred retry wakes.
- **Multi-turn refinement.** Chat and API follow-up reuse the same task
  record, but each rerun starts a fresh root execution. An optional
  execution-local refinement overlay is appended to the effective goal.
  Task-scoped projections honor `output_mode`: `accumulate` (default) or
  `overwrite`. Chat `refine_task` uses overwrite.
- **Agent path.** Autonomous and trigger-driven work also uses the
  orchestrator. Chat-inline cycles call
  `execute_agentic_direct_with_outcome()`. Other goal sources call
  `process_with_strategy()`. There is no `create_and_run_task()` helper.

The shared execution core is the direct agentic loop. The pipeline is
planning-only. Planning Attention cards are reconciled from the authoritative
latest plan. Live qualification: `make test-preplan-flow-live-eval`,
`make eval-preplan-flow-live-interactive`.

---

## 1) Runtime Topology

### Active services / crates

| Crate | Role |
|---|---|
| `magician` | Planning, orchestration, agent runtime, storage, execution |
| `magician-api` | HTTP/WebSocket handlers (`web_api.rs`, `task_api_v3.rs`, `websocket_handler.rs`, …) |
| `magician-bin` | Process entry, Actix server, route mounting |
| `magicutor` | Browser action execution service |
| `tool-runtime-core` | Capability loading, schema validation, lexical/category ranking |
| `runtime-core` | Shared interfaces/types |
| `magicllm` | Provider-agnostic LLM router |
| `magic-supervisor` | Process supervision for bots/daemons |
| `ui/unified-ui` | Product UI |

### High-level data flow

```text
Client / UI
    |
    v
Magician API (:3002)   magician-bin + magician-api
    |
    +-- V3 Task API ---- POST /api/magician/v3/tasks/{id}/plan|execute
    |                         |
    |                         v
    |                   PlanningOrchestrator::run()   (PlanGraph only)
    |                         |
    |                         v
    |                   execute_agentic_direct_with_outcome()
    |
    +-- V2 Execution API  POST /api/magician/v2/executions  (skip_planning → direct)
    |
    +-- Agent API -------- POST /api/magician/v2/agents/{id}/trigger
                              |
                              v
                        AgentRuntime.admit_trigger_in_scope()
                              |
                              v
                        trigger_goal_awaitable_with_scope_and_overrides()
                              |
                              +-- GoalSource::ChatInline
                              |      → execute_agentic_direct_with_outcome()
                              +-- otherwise
                                     → process_with_strategy()
                                           (planning pipeline, then later execute)

Both lanes share:
    execute_agentically() / execute_agent_cycle()
        → Magicutor (:3003)  |  native file/shell/http  |  PackCapabilityProvider
        → scoped V3 storage
        → RuntimeTransportBroadcaster → WebSocket / SSE
```

Both lanes reach `MagicianV2Orchestrator`. The agent lane provisions a V3
task shell (`ArtifactV2Service::create_task_with_execution_shell`) and a
cycle execution before calling the orchestrator. It does not bypass the
orchestrator.

### Path-agnostic execution core

`AgenticContext` carries identity that selects lifecycle events
(execution/task vs agent), but the observe-decide-execute loop is the same.

```text
AgenticContext
  goal, success_criteria, hint_action
  max_iterations, max_repeated_actions
  execution_id, plan_id, step_id
  agent_id?, goal_id?, cycle_id?
  trust_level, approval_rules
  on_failure (AskUser | Fail)
  continuation_count
  depth / max_delegation_depth (default 2)

Routing key:
  agent_id present → agent:{agent_id}:{goal_id}:{cycle_id}
  otherwise        → {execution_id}:{plan_id}:{step_id}
```

---

## 2) Startup and Initialization

### 2.1 Boot sequence

`magician-bin/src/main.rs`:

1. Parse CLI (`host` default `127.0.0.1`, `port` default `3002`,
   `config` default `tool-runtime-config.yaml`).
2. Load tool-runtime config and initialize in-process `LocalToolServices`.
3. Build `MagicianService` via `MagicianServiceBuilder`.
4. Create one process-wide `FullPauseStore` and load pending pauses from
   disk (orphan pauses whose execution dir is gone are reaped).
5. Build shared agent API services against the resolved workspace layout.
6. Wire the definition store onto the orchestrator and hydrate definitions.
7. Spawn GAUI delta emitter, agent-update journal, feed-stall watchdog,
   memory eval/utility workers.
8. Register resume / wake dispatchers (`WakeUpQueue`).
9. Start the Actix server.

### 2.2 MagicianServiceBuilder wiring

`magician/src/magician_core/builder.rs`:

- env files from the runtime config path (`.env.development`, `.env`)
- `MagicianConfig` from the active `magician-config.yaml` (validated YAML)
- `FileV2Store` over `ArtifactV2Workspace`
- `RuntimeTransportBroadcaster` with
  `DEFAULT_RUNTIME_TRANSPORT_CAPACITY` **8192**, storage-backed
  replay, HITL lifecycle persistence, and a workspace event-log writer
  subscribed before any producer is built
- `MagicutorClient` from `execution.magicutor_base_url` + timeout + optional
  API-key env
- `OperationLlmRouter` (operation → profile through `magicllm`)
- prompt storage/manager
- ask-loop stack (clarifier, pause/resume, batch/confidence/rewriter,
  history, metrics)
- `V2ToolMatcher`
- `MagicianV2Orchestrator`

### 2.3 Agent infrastructure wiring

Initialized once per process (not per Actix worker):

- `AgentDefinitionStore` — file-backed CRUD, scoped + system templates
- `AgentRuntime` — definitions, per-dispatch mutexes, trigger queue,
  circuit/feedback/evaluation interpreters, optional
  `v2_orchestrator`, `WakeUpQueue`, scoped schedulers, cancel tokens,
  `goal_cycles`, feedback-injection cache, `AgentMemoryResolver`
- `AgentScheduler` — per-scope, persisted under the agent_runtime root.
  `rebuild_from_definitions()` only prunes stale agent entries; new
  schedules are created from `Task.schedule`, not from agent YAML
- `MuijStorage` / `MuijDeltaEmitter` / `MuijDocumentCache`
- `FullPauseStore` — shared execution and agent pause states
  (`AGENT_KEY_PREFIX` `"agent:"`)

---

## 3) Configuration Surfaces

```text
tool-runtime-config.yaml          magician-config.yaml
  registry.paths                    llm.router.profiles
  validation.strict                 llm.router.operation_mapping
  semantic_search                   execution.magicutor_base_url
                                    execution.shell_sandbox / file_sandbox
                                    execution.on_failure (ask_user | fail)
                 \                  /
                  merged runtime config
                 /                  \
AgentDefinition YAML              Task.schedule (TaskSchedule)
  persona, tools, constraints       cron | interval | once | event
  trust, memory, circuit            timezone, missed_fire, concurrency
  autonomous_config (Personal)      max_runs, paused
```

There is no workflow YAML surface in this runtime.

### 3.1 Tool runtime config

`tool-runtime-config.yaml`:

- `registry.paths`: defaults to `[]` in this repo; compiled/local registry
  tools still come from the tool-runtime registry
- `validation.strict: true`
- `validation.allow_unknown_fields: false`
- `semantic_search.enabled: true` (interface name kept; ranking is
  lexical/category)

Live capability packs: `scopes/<principal>/<workspace>/capabilities/packs/`.

### 3.2 LLM routing + execution policies

Active `magician-config.yaml` (`magician/src/config.rs`):

- `llm.router.profiles` / `llm.router.operation_mapping`
- `execution.magicutor_base_url`
- `execution.shell_sandbox` / `execution.file_sandbox`
- `execution.on_failure` (`AskUser` default, or `Fail`)

### 3.3 Capability / tool surface

**Native `ExecutableAction` variants** (`execution/actions.rs`):
`File`, `Http`, `Bash`, `DuckDb`, `Pack`, `SpawnSubGoal`,
`DelegateToAgent`, `HandoverToAgent`, `SleepUntil`.

**YAML packs** execute through `PackCapabilityProvider`
(`execution/pack_provider.rs`) with
`ImplementationType::{Composite, Compiled, Primitive, Command}`.

**Compiled providers** (examples): `SearchCapabilityProvider`,
`TaskStateProvider` (system-injected for task-backed executions even when
the owning agent omits it), treasurer (only when a secret broker/vault is
configured), `surface_publish` (needs `DurableArtifactStore`).

`task_state` is preserved across pause/resume and owner-profile
transitions.

**Action type restriction** (`allowed_action_types` on
`AgenticContextOverrides`): derived from the agent definition's `tools`
via `derive_allowed_action_types`. `DelegateToAgent` and `SpawnSubGoal`
are always allowed. Direct/scheduled runs without a PlanGraph start in
bash mode (no eager Chromium); the LLM launches the browser on demand.
`execution_mode` only seeds the initial `EnvironmentState`; it does not
restrict later actions. `EnvironmentState` variants:
`Uninitialized | Browser | Filesystem | Http | Shell`.

### 3.4 Agent definition surface

Per-agent YAML (`magician/src/magician_v2/agents/types.rs`,
`deny_unknown_fields`). Full field reference:
[`agents/agent-definition-reference.md`](agents/agent-definition-reference.md).

Agents carry no goals or triggers; scheduling lives on `Task.schedule`
(`TaskSchedule` in `storage/task_models.rs`).

```text
AgentDefinition
├── agent_id, version, name, aliases, wake_spellings
├── description, persona
├── kind: AgentKind { Personal, Worker }
├── disabled, is_primary, onboarding_completed (legacy round-trip)
├── app_tool: Option<AgentAppToolContract>
├── tools / excluded_tools / denied_tools / denied_tool_params
├── browser_transports          (empty = all transports)
├── constraints: AgentConstraints
│     max_iterations            default 4000
│     max_tokens_per_cycle      default 20_000_000
│     max_consecutive_failures  default 3
│     requires_approval: Vec<ApprovalRule>
│     approval_ttl_secs         default 86_400
│     coordination.max_delegation_depth  default 2
│     allow_self_modification
│     max_duration_secs         (overrides GOAL_PIPELINE_TIMEOUT_SECS = 3h)
├── trust_level: TrustLevel     (newtype over String)
├── memory_tiers / memory_consolidation
├── prompt_pipeline, circuit_breaker, feedback_loops
├── notification_rules, retention, llm_routing
├── strategy: StrategyPreference  default Fixed("atomic_composition")
├── state_machines
├── principal / workspace
├── autonomous_config: Option<AutonomousConfig>   (Personal only)
│     schedule (cron), focus_areas[], max_tasks_per_cycle=3, max_steps_per_plan=10
├── social_persona, harness: Option<HarnessConfig>
├── readable_agents, default_personality
├── user_memory_isolation: Shared | FullyIsolated
├── delegation_targets          (empty = none; "*" = all eligible)
├── invocation_policy: AgentInvocationPolicy
├── auto_surface_policy
└── chat_inline: Option<ChatInlinePolicy { Off, Auto, Confirm }>
```

Autonomous work uses `AutonomousConfig`
(`agents/autonomous_goal.rs`: `build_autonomous_goal`,
`filter_focus_areas_by_schedule`). Goal origin is `GoalSource { User,
Schedule, ChatInline }`.

Default memory tiers (`agents/memory_defaults.rs`): `entities`, `insights`,
`recent_activity`, `archive`, `task_progress`, `environment_knowledge`.

---

## 4) Storage Model

Live data root is `$MAGICIAN_ROOT_DIR` (default `~/MagicianNotes`), not
the git seed `magician_data_v3/`. Paths below are relative to that root.

```text
<runtime_root>/
  scopes/<principal>/<workspace>/
    tasks/<task_id>/                          (or internal_tasks/<id>/)
      executions/<execution_id>/
        execution.json                        FileV2Store document
        state.json                            V3 ExecutionRecord
        outputs/                              immutable per-run files
        pipeline/store.json                   planning ArtifactStore
        events.jsonl                          canonical runtime events
      outputs/                                task-scoped latest-view
    agent_runtime/
      agents/<agent_id>/
        definition.agent.yaml
        definitions/v{n}.agent.yaml
        goal_cycles.json                      AgentGoalRecord map
        tiers/  episodes/  corrections.jsonl
        user_memory/  evidence.json  state/
      scheduler_state.json
      .scheduler_state.write.lock
      approvals/  proposals/
    capabilities/packs/
    resource_authority/
  system/                                     templates (read-only seed)
    agent_templates/agents/<name>/definition.agent.yaml
    trust_policy_templates/
```

System scope agent runtime:
`scopes/system/system/agent_runtime/agents/`.

### 4.1 Execution storage

`magician/src/magician_v2/storage/file.rs` (`FileV2Store`):

- per-execution document:
  `scopes/{principal}/{workspace}/tasks/{task_id}/executions/{execution_id}/execution.json`
- locking via `DashMap<String, Arc<Mutex<()>>>` (`execution_locks`) plus
  cross-process `fs2` file locks
- atomic writes: temp file → rename

Each document stores execution metadata/status, turns, slots, state
snapshots, and the clarification session.

### 4.2 Agent definition storage

`agents/definition_store.rs` + `agents/storage.rs`:

- active definition: `definition.agent.yaml`
- version history: `definitions/v{n}.agent.yaml` (not `.versions/`)
- optimistic concurrency: `expected_version` required on updates
- version 1 enforced on creation

### 4.3 Episode / tier storage

Scoped V3 `agent_memory_dir` **is the agent directory itself** (no nested
`memory/`):

- episodes: `agent_runtime/agents/{id}/episodes/`
- tiers: `agent_runtime/agents/{id}/tiers/{tier_name}.json`
- corrections: `agent_runtime/agents/{id}/corrections.jsonl`
- isolated user memory: `agent_runtime/agents/{id}/user_memory/`

### 4.4 Scheduler / pause

- scheduler state: `agent_runtime/agents/scheduler_state.json` with a
  30s write lock (retry 10–250ms)
- pause states: `FullPauseStore` (no separate agent pause directory)

### 4.5 Task output mode

`artifact_v2/models.rs`:

```text
TaskOutputMode { Accumulate (default), Overwrite }
```

Accumulate suffixes the root execution id so delegated children land in
the same task-visible run bucket. Overwrite keeps stable latest-wins
filenames; terminal reduction prunes stale accumulated primary output
refs.

---

## 5) Tool Discovery and Matching Flow

Shared by both paths.

```text
capabilities/**/*.yaml          user request / agent goal
        |                                |
        v                                v
registry load + validation      route hints + lexical score
        |                                |
        +---------------+----------------+
                        v
               candidate shortlist
               (max_llm_candidates clamped to 1..=4)
                        v
               LLM disambiguation
                        v
               selected tool + confidence
```

### 5.1 Registry loading

`tool-runtime-core/src/registry/mod.rs`: recursive YAML load, strict
schema when configured, canonical files override `versions/*`,
equal-priority duplicates rejected in strict mode.

### 5.2 Matcher pipeline

`magician/src/magician_v2/tool_matcher/service.rs` (`V2ToolMatcher`):

1. Infer route hints (`browser`, `files`, `search`, `shell`).
2. Score with route + lexical + optional semantic score.
3. Select top candidates (`max_llm_candidates`, clamped `<= 4`).
4. LLM disambiguation (`LlmEvaluator`).
5. Return best tool + confidence.

---

## 6) Execution Entry Points

| | Task / execution path | Agent path |
|---|---|---|
| Entry | User message, `POST /v3/tasks/{id}/plan` then `/execute`, or V2 `skip_planning` | Cron/event/`Task.schedule`, `POST /v2/agents/{id}/trigger`, chat-inline `delegate_to_agent` |
| Planning | `PlanningOrchestrator` → `PlanGraph` | Chat-inline skips planning. Other sources call `process_with_strategy()` (planning). Reusable PlanGraph lookup returns `None` (see §9.7) |
| Execution | `execute_agentic_direct_with_outcome()` | Same orchestrator methods |
| Goal source | Task description + optional refinement overlay | Focus-area / goal description from the agent definition; `GoalSource` stamped on `AgentGoalRecord` |
| Memory after run | Execution turns + V3 artifacts | `AgentGoalRecord` + `EpisodeRecord` + memory tiers |

```text
Task / execution path                      Agent path
=====================                      ==========
User message / POST …/execute              Trigger / POST …/trigger
        |                                          |
        v                                          v
POST /v3/tasks/{id}/plan (optional)        admit_trigger_in_scope()
PlanningOrchestrator::run()                        |
        |                                          v
        |                                  create_task_with_execution_shell()
        |                                  write AgentGoalRecord in_progress
        |                                          |
        v                                          v
POST /v3/tasks/{id}/execute                ChatInline?
ArtifactV2Service::start_execution()         yes → execute_agentic_direct_with_outcome()
        |                                    no  → process_with_strategy()
        v                                          |
execute_agentic_direct_with_outcome() <------------+
        |
        v
execute_agentically() / execute_agent_cycle()
```

### 6.1 Path A: Execution / task execution (user-driven)

Entry points (mounted in `magician-bin/src/main.rs`, handlers in
`magician-api/src/task_api_v3.rs` and `web_api.rs`):

- `POST /api/magician/v3/tasks/{id}/plan` — start planning
- `POST /api/magician/v3/tasks/{id}/plan/approve|reject|replan`
- `POST /api/magician/v3/tasks/{id}/execute` — `execute_task_v3_handler` →
  `ArtifactV2Service::start_execution`
- `POST /api/magician/v2/executions` with `skip_planning: true` — direct
  agentic, no planning pipeline

`process_with_strategy()` helpers:

1. `prepare_execution_and_turn()` — execution/turn setup, correlation ID
2. `build_and_execute_pipeline()` — `PlanningOrchestrator` with the seven
   production pipeline agents, `PIPELINE_MAX_ITERATIONS = 20`
3. `handle_pipeline_outcome()` — persist analysis, emit events

An ask-loop gate before pipeline construction returns early if a prior
clarification session is still collecting answers.

**Planning.** `IntentClassifierAgent` runs first. Non-task intents
(status, cancel, resume) terminate via `rule_non_task_complete`. Task
intents produce a `PlanGraph`. `enrich_depends_on()` derives dependencies
from edges; `check_no_cycles()` validates with Kahn's sort.

**Execution handoff.** `trigger_execution` / `start_execution` looks for a
PlanGraph, renders it into bounded runtime context when present, and
calls `execute_agentic_direct_with_outcome()`. It does not convert
PlanGraph into taskplan markdown. WakeUpQueue and the web API wakeup loop
do not gate on `has_plan`. If the task has no active execution, the
scheduler creates and binds one.

**Rerun.** `POST /tasks/{id}/execute` always creates a fresh root
`execution_id` after reconciling any stale active marker. The only hard
rejection is an actually active run. Same-execution resume
(`Paused`/`Deferred` on the current root) preserves that execution's
runtime context.

**Refinement overlay.** Optional string; effective goal = stored
description + overlay. The task manifest is not rewritten.

**Prior-output carry-forward.** The executor walks the task execution
tree, reloads `downloaded_file` / `tool_output_file` records, dedupes by
stable display name, caps prompt-visible seeded artifacts at 10.

**Tool-produced file capture.** After a pack action completes, the
executor scans structured results, copies discovered files into both
execution-scoped and task-scoped `outputs/`, registers
`tool_output_file`, and emits `ArtifactCreated`.
`is_allowed_tool_output_capture_path` (`executor.rs`) accepts the process
CWD, `std::env::temp_dir()`, `/tmp`, and `/private/tmp`.

**Output serving.**
`GET /api/magician/v3/tasks/{task_id}/outputs/{artifact_path:.*}` with
inferred inline media types. `MAX_SELECTED_ARTIFACTS = 30`
(`artifact_v2/synthesis.rs`). Promoted media files become
`audience=user`, `role=user_media` refs on `FinalizedOutputs.media_outputs`.

**Delegation resume.** Task-backed direct runs resume automatically after
delegated children finish (`WaitingChildren` → synthesized child summary
→ continue the same parent execution). Timed-out children persist a
partial timeout summary.

**Terminal recovery.** Cancellation is terminal without answer synthesis.
Startup recovery clears stale `synthesis_pending` flags for
cancelled/canceled records. Task-card summary sidecars share a two-permit
background lane.

**Execution `WaitingState`** (`storage/models.rs`):
`Planning`, `PlanningComplete`, `Runnable`, `Executing`, `Sleeping`,
`WaitingChildren`, `WaitingUser`, `Completed`, `Failed`, `Cancelled`,
`Paused`.

### 6.2 Path B: Agent execution (trigger-driven)

```text
Cron / Task.schedule / event / POST /agents/{id}/trigger / chat-inline
        |
        v
AgentRuntime.admit_trigger_in_scope(principal, workspace, agent_id, goal_id, …)
        |
   StartNow | Queued | Duplicate | QueueFull (cap 16)
        |
        v
trigger_goal_awaitable_with_scope_and_overrides()
  resolve focus-area goal text
  create_task_with_execution_shell()
  write AgentGoalRecord { status: "in_progress" } → goal_cycles.json
  spawn_execution_job:
      ChatInline → execute_agentic_direct_with_outcome()
      else       → process_with_strategy(seed_plan_graph=None today)
  record_goal_outcome_and_decide_circuit_in_scope()
  complete_active_cycle_in_scope()  (promotes next queued trigger)
```

Admission is keyed by scoped dispatch key
`(principal, workspace, agent_id, goal_id)` — **not** one queue per
agent globally.

| Constant | Value |
|---|---|
| `MAX_PENDING_TRIGGERS_PER_SCOPE` | 16 |
| `RECENT_COMPLETED_TRIGGER_KEYS_PER_SCOPE` | 256 |
| `MAX_GOAL_CYCLE_RECORDS_PER_AGENT` | 512 |
| `GOAL_PIPELINE_TIMEOUT_SECS` | 10800 (3h), overridable per agent |

Cycle id: `scope:{hex(principal)}:{hex(workspace)}::{agent_id}::{goal_id}::{trigger_seq}`
when a scope is present, else `{agent_id}::{goal_id}::{trigger_seq}`.

Cancel tokens are keyed by the deterministic automation task id
`legacy:{agent_id}:{goal_id}` (scoped).

**UI surfaces** for this path: `/crew`, `/crew/[id]`,
`/crew/[id]/memory/[tier]`, `/triggers`, `/approvals`.

### 6.3 Shared execution core

`magician/src/magician_v2/execution/agentic/executor.rs`

```text
execute_agent_cycle(AgenticContext)
    OBSERVE → DECIDE (LLM) → EXECUTE
        File | Http | Bash | DuckDb | Pack (registry)
        | SpawnSubGoal | DelegateToAgent | HandoverToAgent | SleepUntil
        | browser via Magicutor
    VERIFY against goal
        achieved → break
        else → next iteration (until budget)
```

Both paths use the same `OperationLlmRouter`, `MagicutorClient`, native
executors, `CapabilityRegistry` / `PackCapabilityProvider`, and
observation/verification hooks. The browser client is always available
regardless of initial `EnvironmentState`.

**`AgenticOutcome`** (`execution/agentic/types.rs`):

- `Success { final_state, iterations_used, artifacts }`
- `Failed { reason, last_state, iterations_used }`
- `MaxIterationsReached { last_state, iterations_used, pause_state? }`
- `LoopDetected { detection_type, repeated_action, recommendation, last_state, … }`
- `WaitingForUser { question, input_type, hint, pause_state, escalation_trigger?, … }`
- `WaitingForConfirmation { action_summary, reason, pause_state, … }`
- `PausedByUser { pause_state, iterations_used }`
- `WaitingForChildren { child_execution_ids, last_state, pause_state?, … }`
- `BudgetExhausted { dimension, last_state, pause_state? }`
- `CannotProceed { reason, last_state, iterations_used }`
- `Sleeping { wake_at, paused_state?, … }`

#### 6.3.1 SpawnSubGoal — depth-bounded recursion

`Decision::SpawnSubGoal { goal, budget }` recurses into
`execute_agentically()` with `depth += 1`.

- Max depth: `max_delegation_depth` (default 2)
- Budget: clamped to the parent's remaining iterations
- Parent `approval_rules` propagate
- Sub-goals do **not** create separate Task records; artifacts roll up
  into the parent (`history.artifacts.extend`, shared durable store)

#### 6.3.2 DelegateToAgent — cross-agent child executions

`Decision::DelegateToAgent { targets }` spawns a **new** `execution_id`
per target via `RuntimeDelegationDispatcher` →
`MagicianV2Orchestrator::create_delegation_execution`.

After the V2 in-memory `ExecutionRun` write,
`ArtifactV2Service::record_runtime_child_execution_best_effort` mirrors a
V3 `ExecutionRecord` (`relationship_type: "delegate"`) and updates the
parent `refs.delegations`. `Ok(false)` means the parent is not V3-backed
(silent no-op). Genuine IO errors warn.

`relationship_type` has exactly two values, `RELATIONSHIP_TYPE_ROOT`
(`"root"`) and `RELATIONSHIP_TYPE_DELEGATE` (`"delegate"`), both defined in
`artifact_v2/models.rs`. `execution_record_from_runtime` is the only writer
and picks between them on `parent_execution_id`. Compare against the
constants, never a literal — a drifted literal makes lineage guards silently
reject every delegated child.

#### 6.3.3 Context enrichment before execution

`execute_agentic_direct_with_outcome()` builds `AgenticContext` before the
loop:

- Durable artifact context (`## Shared Workspace`) when
  `DurableArtifactStore` is configured
- `task_state` pre-load for task-backed runs (runtime-default capability)
- Linked task artifacts (`## LINKED TASK ARTIFACTS`) from `depends_on` /
  pinned `linked_task_inputs`

### Resource authority (spend-gated execution)

When a pack declares `SpendDeclaration`, lowering produces
`MaybeGatedAction::Gated`. Live dispatch settles the gate through
`spend_session::admit`:

1. **Reserve** — freeze, token status/expiry, budget, system ceiling,
   token ceiling, velocity. Journal: budget → reserved.
2. **Execute** the inner action (locks released during execution).
3. **Commit or rollback** — reserved → expense, or back to budget.

State on `ActionExecutors`: `ResourceLedger`, `TokenStore`,
`SystemCeilings`, `SystemFreezeState`.

REST at `/api/magician/v2/resource-authority/`
(`magician-api/src/resource_authority_api.rs`: ceilings, bootstrap/withdraw,
tokens, ledger, reservations, freeze, period-close, refund, credit). See
[`resource-authority-api.md`](resource-authority-api.md).

### 6.4 Interaction points

Both paths share one `RuntimeTransportBroadcaster` and the same approval
gates (user confirmation on the task path; trust level +
`requires_approval` on the agent path).

App-local workflow markdown under agent templates
(`harness-sre/app/workflows/`, learning SKILL workflows) is not a runtime
orchestration engine; there is no workflow dispatcher.

### 6.5 User escalation on failure

When `execution.on_failure: ask_user` (default), permission walls HITL;
generic give-up is terminal. See
[`execution/USER_ESCALATION_ARCHITECTURE.md`](execution/USER_ESCALATION_ARCHITECTURE.md).

- Auth/permission blockers → `escalate_to_user()` (`reauth` / `permission`)
- Budget / max-iterations → Continue HITL
- Everything else → terminal `Failed` / `CannotProceed`
- `on_failure: fail` → permission walls are also terminal

Safety: `MAX_GUIDANCE_LENGTH = 500`, `MAX_CONTINUATION_COUNT = 5`
(`magician-api/src/web_api.rs`). Cap reached: "Keep Trying" refused;
Done/Cancel remain. Escalation UI is the execution-panel / HITL surfaces.

---

## 7) Clarification / Ask-Loop Flow

Runs inside `process_with_strategy`. Agent chat-inline and V2
`skip_planning` bypass it.

```text
planning detects missing required inputs
        → generate/curate question batch
        → persist clarification session
        → WaitingUser
        → user responds
        → interpret answer + update slot/confidence
              incomplete → stay WaitingUser
              complete   → resume planning
```

Components (`magician/src/magician_v2/ask_loop/`): `AskLoopApi`,
`ClarifierLibrary`, `PauseResumeManager`, `ResumeTriggerService`,
`QuestionBatchTracker`, `PlanConfidenceTracker`, `QuestionRewriter`.

Primary endpoints (`magician-api/src/task_api_v3.rs`, mounted under
`/api/magician/v3`):

- `POST /tasks/{task_id}/plan/clarifications/{question_id}/respond`
- `POST /tasks/{id}/plan/clarifications/resume`
- `GET  /tasks/{id}/plan/clarifications`
- `GET  /tasks/{id}/plan/clarifications/pending`
- `GET  /plans/clarifications/pending`

---

## 8) Planning Pipeline Internals

`PlanningOrchestrator::run()` (`pipeline/orchestrator.rs`) is a
router-driven loop. LLM router fallback is disabled (`TieredRouter::new`
passes `llm: None`). Every message enters the pipeline; intent
classification is the first agent.

`PIPELINE_MAX_ITERATIONS = 20` (`v2_orchestrator.rs`); `default_rules()`
returns 17 rules (§8.1).

```text
PlanningOrchestrator::run()   (bounded for-loop, max 20)
        |
        v
TieredRouter.route(store, context)   first match wins, no I/O
        |
   NextStage | Complete | Pause | Retry | Error
        |
        v
agent.execute() writes artifacts → loop
```

### Flow path: new task (happy path)

| Iter | Rule | Agent | Artifacts |
|---|---|---|---|
| 1 | cold_start | IntentClassifierAgent | IntentClassification, QueryAnalysis |
| 2 | needs_slot_extraction | SlotExtractorAgent | SlotGraph |
| 3 | needs_elicitation | ElicitorAgent | ElicitationResult (`needs_clarification: false`) |
| 4 | slots_resolved | QueryRewriterAgent | ClarifiedTask |
| 5 | ready_to_plan | PlannerAgent | PlanGraph |
| 6 | planning_complete | — | Complete |

Execution is a later `/execute` (or skip-planning) phase.

### Flow path: non-task intent

1. cold_start → IntentClassifier (`requires_pipeline: false`)
2. non_task_complete → Complete

`handle_pipeline_outcome()` returns
`StrategyProcessingResult::for_non_pipeline_exit()`.

### Flow path: clarification needed

1–3 as happy path, but `needs_clarification: true` → Pause.
User answers via `/plan/clarifications/{q_id}/respond`.
Resume: user_answer_needs_interpretation → AnswerInterpreterAgent, then
slots_resolved → rewriter → planner → complete.

### Flow path: resume modes

| Mode | First matching rule |
|---|---|
| `"answer"` | user_answer_needs_interpretation |
| `"light_slot_update"` | interpreted_answer_light_update |
| `"full_replan"` | resume_full_replan → slot-extractor |
| `"partial_replan"` | resume_partial_replan |

`IntentClassification` and `QueryAnalysis` are never purged. Rules
cold_start / non_task_complete skip when `resume_mode` is set.

### 8.1 TieredRouter rule chain

`pipeline/router.rs` `default_rules()`, first match wins:

| # | Function | Decision |
|---|---|---|
| 1 | `rule_resume_full_replan` | `NextStage(system:slot-extractor)` |
| 2 | `rule_resume_partial_replan` | rewriter / downstream |
| 3 | `rule_user_answer_needs_interpretation` | `system:answer-interpreter` |
| 4 | `rule_interpreted_answer_needs_replan` | rewriter / slot-extractor |
| 5 | `rule_interpreted_answer_light_update` | elicitor |
| 6 | `rule_permanent_agent_error` | `Error` |
| 7 | `rule_service_error_retry` | `Retry` (no iteration-budget consume) |
| 8 | `rule_cold_start` | `system:intent-classifier` |
| 9 | `rule_non_task_complete` | `Complete` |
| 10 | `rule_needs_slot_extraction` | `system:slot-extractor` |
| 11 | `rule_needs_elicitation` | `system:elicitor` |
| 12 | `rule_needs_clarification` | `Pause` (empty `slot_ids` → `Error`) |
| 13 | `rule_slots_resolved` | `system:query-rewriter` |
| 14 | `rule_ready_to_plan` | `system:planner` |
| 15 | `rule_refinement_chain_complete` | `system:plan-patcher` |
| 16 | `rule_needs_refinement` | slot-extractor for first Weak step |
| 17 | `rule_planning_complete` | `Complete` |

Refinement budget: `MAX_REFINEMENT_ROUNDS = 2`,
`MAX_REFINEMENT_USER_PAUSES = 2`. Exhausted Weak steps fall through to
`rule_planning_complete` and execute as-is.

Reusable PlanGraph reuse is **not** a router concern.

### 8.2 System agents

`MagicianV2Orchestrator::build_pipeline_agents` registers **seven**
agents (`system:scheduler` is not in this map).

| Agent ID | Rust type | Requires | Produces |
|---|---|---|---|
| `system:intent-classifier` | `IntentClassifierAgent` | — | `IntentClassification`, `QueryAnalysis` |
| `system:slot-extractor` | `SlotExtractorAgent` | `QueryAnalysis` | `SlotGraph` |
| `system:elicitor` | `ElicitorAgent` | `SlotGraph` | `ElicitationResult` |
| `system:answer-interpreter` | `AnswerInterpreterAgent` | elicitation + user answer | `InterpretedAnswer` |
| `system:query-rewriter` | `QueryRewriterAgent` | slots + analysis | `ClarifiedTask` |
| `system:planner` | `PlannerAgent` | `ClarifiedTask` | `PlanGraph` |
| `system:plan-patcher` | `PlanPatcherAgent` | refinement chain | patched `PlanGraph` |

`SchedulerAgent` (`system:scheduler`) still exists as the schedule
evaluator used by `WakeKind::Scheduled` on `WakeUpQueue`; it is not a
planning-pipeline stage.

`PlannerAgent` delegates to `AdaptiveStrategySelector`.
`consumer_mode: true` (production default) forces `AtomicComposition` and
blocks GuidedSearch. See [`plan-flow.md`](plan-flow.md).

### 8.3 PipelineContext and ArtifactStore

**`PipelineContext`** (`pipeline/agent.rs`): `chain_id`, `cycle_id`,
`workflow_id` (legacy string field, not a workflow runtime), `query`,
`iteration`, `question_id`, `resume_mode`, `user_answer`,
`schedule_context`, `run_started_at`, `refinement_step_id`,
`refinement_rounds`, `refinement_user_pauses`, `task_id`, `execution_id`,
`principal`, `workspace`, `max_delegation_depth`.

**`ArtifactStore`**: append-only; `put`, `latest_of_type`,
`snapshot` / `restore_from_snapshot`. Persisted by
`FilesystemExecutionPipelineStore` at
`…/executions/{execution_id}/pipeline/store.json` with artifact-count,
JSON-node, retained-depth, and 64 MiB encoded-document ceilings.

Artifact types: `IntentClassification`, `QueryAnalysis`, `SlotGraph`,
`ElicitationResult`, `InterpretedAnswer`, `ClarifiedTask`, `PlanGraph`,
`AgentError`.

### 8.4 Suspension and resume

`Pause` → `PipelineSuspension` persisted; execution `WaitingUser`.
Resume restores the snapshot and continues. Shared `FullPauseStore`.

| Resume mode | Purged | Preserved |
|---|---|---|
| `full_replan` | SlotGraph, ElicitationResult, InterpretedAnswer, ClarifiedTask, PlanGraph, AgentError | IntentClassification, QueryAnalysis |
| `partial_replan` | ElicitationResult, InterpretedAnswer, ClarifiedTask, PlanGraph, AgentError | IC, QA, SlotGraph |
| `light_slot_update` / `answer` / other | AgentError | all others |

### 8.5 Planner backends

- **Atomic composition** (default) —
  `magician/src/magician_v2/strategy/atomic_composition.rs`
- **Guided search** — `strategy/guided_search.rs`, gated off when
  `consumer_mode: true`

### 8.6 Agent trigger goal shaping

`agents/runtime.rs` + `orchestrator/v2_orchestrator.rs`:

- strategy preference `fixed` / `ordered` / `auto_select`
  (`AUTO_SELECT_STRATEGY_CANDIDATES = ["atomic_composition", "guided_search"]`)
- prompt pipeline / trust context / LLM routing overrides
- chat-inline `delegate_to_agent` always uses the direct agentic path
  with the shared override builder; the tool call returns after start;
  progress fans out on `RuntimeTransportBroadcaster` keyed by
  `delegate_execution_id` (`payload.chat_delivery_kind: "inline_delegate"`).
  Inline delegates default to `lifecycle: internal` (swept with their chat
  session); `track_as_task: true` makes the task `persistent`. See [`chat-mode.md`](chat-mode.md)
  and [`v2-websocket-events.md`](v2-websocket-events.md).

---

## 9) Agent Lifecycle

```text
POST /api/magician/v2/agents  or YAML load
        → AgentDefinitionStore.create_definition (version 1)
        → AgentRuntime.upsert_definition
        → AgentScheduler.rebuild_from_definitions (prune only)
        → idle until Task.schedule / trigger / chat-inline
        → admit_trigger_in_scope
        → trigger_goal_awaitable_with_scope_and_overrides
        → AgentGoalRecord + EpisodeRecord + memory / circuit / feedback
        → complete_active_cycle_in_scope → idle
```

### 9.1 Definition loading and hot-reload

`agents/definition_store.rs`: scoped/system `AgentStorage` root,
`upsert_definition()` hot-reload, `expected_version` concurrency.

### 9.2 Trigger admission and queueing

`agents/runtime.rs`: serial execution per scoped `(agent_id, goal_id)`
dispatch key; `TriggerAdmission::{StartNow, Queued, Duplicate, QueueFull}`;
limits in §6.2.

### 9.3 Episode recording

`agents/memory.rs` `EpisodeRecord`: `episode_id`, `agent_id`, `goal_id`,
`trigger_seq`, `trigger`, timestamps, `EpisodeOutcome`, `actions_taken`,
`observations`, `memory_updates`, `strategy_summary`, `context_at_start`,
`artifact_output`.

`EpisodeOutcome`: `GoalAchieved`, `PartialProgress`, `Failed`,
`UserIntervened`, `BudgetExhausted`, `Paused`, `CircuitOpen`.

Episodes are persisted to disk and **not** broadcast as realtime events;
the UI fetches them over REST.

### 9.4 Memory tiers and consolidation

Default tiers and rules live in `agents/memory_defaults.rs`. Default
consolidation rules include `extract_entities`, `extract_insights`,
`summarize_recent_activity`, `update_task_progress`, `distill_insights`,
`archive_old_episodes`, `expire_old_episodes`,
`extract_environment_knowledge`, `promote_to_user`. Interpreted by
`memory_consolidator.rs`. Correction categories: Targeting, Timing,
Action, Preference, Safety.

### 9.5 Circuit breaker

`CircuitBreakerPolicy`: `thresholds`, `recovery` (`UserReset` |
`TimeBased { cooldown_hours }`), `per_goal_override`, plus
`idle_failure_threshold` / `idle_open_duration_minutes`.

Tracked in `AgentRuntime.circuit_failures`. After each goal pipeline,
`record_goal_outcome_and_decide_circuit_in_scope` runs. An
`OpenCircuit` decision is logged; `trigger_goal_awaitable` still
unconditionally `complete_active_cycle_in_scope` so later triggers are
not stuck as Duplicate. Admission itself does not consult the open
circuit.

### 9.6 Feedback loops

`FeedbackLoopDefinition` + `FeedbackLoopInterpreter`.
`AgentRuntime.feedback_injection_cache` holds transformer output from the
previous cycle, keyed by `agent_id`.

### 9.7 AgentGoalRecord (built)

Written at cycle start (`runtime.rs`) and persisted to
`{agent_dir}/goal_cycles.json`.

```rust
pub struct AgentGoalRecord {
    pub cycle_id: String,
    pub agent_id: String,
    pub goal_id: String,
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub execution_id: Option<String>,
    pub goal_input_hash: String,          // sha256 of goal description
    pub fired_at: DateTime<Utc>,
    pub status: String,                   // in_progress | completed | failed
    pub source: GoalSource,               // User | Schedule | ChatInline
}
```

**PlanGraph reuse:** `get_reusable_plan_graph_in_scope` looks up the
newest completed matching hash **then returns `None`** — fast-path plan reuse
is not live behavior.

---

## 10) Task and Execution Model

- **Task** — product-level work item (`/api/magician/v3/tasks/*`), always
  assigned to an agent
- **Execution** — one run of that task (`execution_id`), with its own
  event log, artifacts, and runtime-context state
- **Agent** — executor (`AgentDefinition`); no goals/triggers on the
  definition. Recurring work is `Task.schedule`

Agents create tasks through the `create_task` tool (same-scope auto-dispatch
when unscheduled; see [`agent-task-dispatch.md`](agent-task-dispatch.md)).

```text
POST /api/magician/v3/tasks/{id}/execute
        → start_execution (fresh root execution_id)
        → render PlanGraph as runtime context if present
        → execute_agentic_direct_with_outcome()
        → task status follows execution outcome
```

Task status: `pending → planning → ready → running →
completed | failed | paused | cancelled`.

Startup recovery:

```text
stale Running  → Ready   (or Deferred if retry_at is still in the future)
stale Planning → Pending (or Ready if an approved, dependency-free plan exists)
retry_at       → re-enqueued onto WakeUpQueue as TaskRetry
```

### 10.1 Cross-task artifact linking

See [`execution/LINKED_TASK_ARTIFACTS.md`](execution/LINKED_TASK_ARTIFACTS.md).

| Field | Mutable? | Purpose |
|---|---|---|
| `depends_on: Vec<String>` | task mutation | durable upstream set (max 50, same workspace) |
| `linked_task_inputs` | execution-scoped | exact refs pinned for one root execution |

Artifact names: `"durable:namespace/name"` or
`"inline:name|content_type|preview"`. Durable store:
`artifacts/durable_store.rs` (atomic rename, `fs2` locking, path
traversal protection). Namespaces include `"task_state"` and `"surfaces"`.

### 10.2 WakeUpQueue

`agents/wake_up_queue.rs` — durable timers, no cron math.

`WakeKind`: `Scheduled`, `TaskSchedule` (legacy recovery), `TaskRetry`,
`ExecutionRetry`, `ChildCompleted`.

Ceilings: 4096 entries, 4 MiB file, 64 concurrent dispatches, 30s claim
lease. Production owner is the web/API dispatcher.

---

## 11) Realtime Events and Observability

```text
execution-path events          agent-path events
        \                      /
         RuntimeTransportBroadcaster (capacity 8192)
                    |
         persist (events.jsonl) + publish
                    |
         GET /api/magician/v2/realtime/ws
         GET /api/magician/v3/events  (NDJSON)
                    |
         UI: agentStore, approvalStore, muijStore, taskStore, …
```

WebSocket (`magician-api/src/websocket_handler.rs`): heartbeat **5s**,
client timeout **30s**, max frame **64 KiB**.

**Execution-path events** include `MessageProcessingStarted`,
`QueryAnalysisCompleted`, `StrategySelected`, `ExecutionStarted` /
`StepStarted` / `StepCompleted`, `ExecutionCompleted` / `Failed` /
`Paused` / `Resumed` / `Cancelled`, `AgenticWaitingForUser` /
`AgenticExecutionCompleted` / `AgenticMaxIterationsReached`,
clarification queued/response/snapshot.

**Agent-path `RuntimeTransportEvent` variants:** `AgentTriggered`,
`AgentCycleStarted`, `AgentCycleCompleted`. Envelope events:
`agent.created/updated/paused/resumed/deleted`, `agent.ui.delta`,
`approval.requested/resolved/expired`, `agent.update` (journaled to
`scopes/<p>/<w>/updates.jsonl`).

Payload shapes: [`v2-websocket-events.md`](v2-websocket-events.md).

Observation artifacts:

- `GET /executions/{execution_id}/observations/{id}/screenshot`
- `GET /executions/{execution_id}/observations/{id}/json`
- storage stats under `/executions/{execution_id}/storage-stats`

---

## 12) Safety and Sandbox Boundaries

```text
action
  → shell sandbox (mode, network, binaries, blocked fragments, cwd)
  → file sandbox (mode, allowed_roots, delete_policy)
  → capability schema validation (strict on load)
  → trust level + ApprovalRule evaluation (agent path)
  → allowed | NeedsApproval | blocked
```

### 12.1 Shell and file sandbox

Configured in `magician-config.yaml`, enforced in native executor paths
before filesystem/shell operations.

### 12.2 Trust levels

`TrustLevel(pub String)` — unknown levels resolve via declarative
policies. Conventional values used in product YAML: `local`, `reviewed`,
`untrusted`, `builtin`.

### 12.3 Approval rules

`agents/approval.rs`, driven by `StateMachineInterpreter`:

```text
ApprovalRule { tool, action, when: { param_matches, url_contains }, ttl_secs }
→ Approved(plan) | NeedsApproval { plan, pending_actions }
```

Tool `"*"` matches any tool. Default TTL 86_400s.

Further per-agent constraint: `tools` / `excluded_tools` / `denied_tools`
/ `denied_tool_params` / `browser_transports` / `invocation_policy`.

---

## 13) Frontend Architecture

`ui/unified-ui/src/routes/(app)/`.

| Route | Role |
|---|---|
| `/tasks` | Task list / execution path |
| `/crew`, `/crew/new`, `/crew/[id]` | Agent list / create / detail |
| `/crew/[id]/memory/[tier]` | Memory tier viewer |
| `/crew/[id]/rules` | Agent rules |
| `/triggers` | Trigger / schedule management |
| `/approvals` | Approval queue |
| `/chat` | Chat |
| `/history` | History |
| `/dashboard` | Dashboard |
| `/briefing`, `/today`, `/feed`, `/attention` | Briefing / today / feed |
| `/vault` | Vault |
| `/channels` | Channels |
| `/memory` | Memory |
| `/budget`, `/runtime/resources` | Resource authority UI |
| `/harness` | Harness |
| `/meetings` | Meetings |
| `/observe` | Observe |
| `/evals`, `/events`, `/debug` | Eval / events / debug |
| `/apps` | Apps |
| `/settings`, `/skills` | Settings / skills |
| `/about` | About |

### Stores

Planning/execution stores include `taskStore`, `agentStore`, `approvalStore`, `muijStore`,
`chatStore`, `attentionStore`, `feedStore`, `resourceAuthorityStore`,
`planModeStore`, `trustPolicyStore`. WebSocket dispatch lives in
`ui/unified-ui/src/lib/realtime/v2-websocket.ts`.

Agent status shown in the UI is **derived** from realtime events
(`AgentTriggered`, `AgentCycleStarted`, `AgentCycleCompleted`) by
`agentStore`; it is not a persisted backend field.

### GAUI / MUIJ

`agent.ui.delta` (`upsert` / `remove` / `reorder`) → `muijStore` → agent
detail. Caps: 1000 tracked agents, 10k reorder IDs, 500 recently cleared.
`MuijComponent`: `id`, `component_type`, `label`, `source`, `query`,
`props`, `static_snapshot`, `children` (max depth 32).

Other UI patterns: event watermarking, optimistic updates, 300ms approval
reconciliation debounce, MUIJ snapshot 2s cooldown / 30s timeout, 5s
task-completion grace.

Plan inspection widgets live next to the app routes
(`PlanGraphView.svelte`); execution inspection is
`/tasks/{id}/execution-panel` plus the HITL/attention surfaces.

---

## 14) Fast Operational Checklist

### Shared infrastructure

1. `magicutor` reachable at `execution.magicutor_base_url`.
2. `tool-runtime-config.yaml` valid; packs under
   `scopes/<principal>/<workspace>/capabilities/packs/`.
3. Active `magician-config.yaml` has LLM profiles + operation mappings.
4. Start magician (`make restart-magician`) and check `/health`.

### Task path

5. Create a task, `POST …/plan`, verify analysis on the latest turn and
   `PlanningComplete` when a plan exists.
6. `POST …/execute`; confirm `Executing`, observations, pause-state, and
   a terminal or waiting-user state.

### Agent path

7. `GET /api/magician/v2/agents` lists hydrated definitions.
8. `POST /api/magician/v2/agents/{id}/trigger` (or wait for
   `Task.schedule`); confirm `AgentTriggered`.
9. Confirm `AgentCycleStarted` / `AgentCycleCompleted` and
   `{agent_dir}/goal_cycles.json`.
10. Confirm episodes under `{agent_dir}/episodes/`.
11. Approval: `requires_approval` → `approval.requested` →
    `POST /api/magician/v2/approvals/{id}/resolve`.
12. GAUI: `agent.ui.delta` → `muijStore`.

---

## Appendix: Key Source File Reference

### Planning pipeline (§8)

| Concept | File |
|---|---|
| `PlanningOrchestrator` | `magician/src/magician_v2/pipeline/orchestrator.rs` |
| `TieredRouter` (17 rules in `default_rules()`) | `magician/src/magician_v2/pipeline/router.rs` |
| `PipelineAgent` / `PipelineContext` | `magician/src/magician_v2/pipeline/agent.rs` |
| `ArtifactStore` | `magician/src/magician_v2/pipeline/artifact.rs` |
| `FilesystemExecutionPipelineStore` | `magician/src/magician_v2/artifact_v2/pipeline_store.rs` |
| `PlanGraph` | `magician/src/magician_v2/strategy/plan.rs` |
| System agents | `magician/src/magician_v2/pipeline/system_agents/` |
| Direct execution entry | `magician/src/magician_v2/orchestrator/v2_orchestrator.rs` |
| Atomic composition | `magician/src/magician_v2/strategy/atomic_composition.rs` |
| Guided search | `magician/src/magician_v2/strategy/guided_search.rs` |
| `SchedulerAgent` (wake path, not planning map) | `magician/src/magician_v2/pipeline/system_agents/scheduler.rs` |

### Core infrastructure

| Concept | File |
|---|---|
| Process entry | `magician-bin/src/main.rs` |
| Service builder | `magician/src/magician_core/builder.rs` |
| Config | `magician/src/config.rs` |
| V3 task API | `magician-api/src/task_api_v3.rs` |
| Web API / agent API | `magician-api/src/web_api.rs` |
| WebSocket | `magician-api/src/websocket_handler.rs` |
| Resource authority routes | `magician-api/src/resource_authority_api.rs` |
| Orchestrator | `magician/src/magician_v2/orchestrator/v2_orchestrator.rs` |
| Agentic executor | `magician/src/magician_v2/execution/agentic/executor.rs` |
| Agentic types (`AgenticOutcome`, `EnvironmentState`) | `magician/src/magician_v2/execution/agentic/types.rs` |
| Pack provider | `magician/src/magician_v2/execution/pack_provider.rs` |
| `ImplementationType` | `magician/src/magician_v2/execution/capability.rs` |
| Agent types | `magician/src/magician_v2/agents/types.rs` |
| Agent runtime / `AgentGoalRecord` | `magician/src/magician_v2/agents/runtime.rs` |
| Agent storage | `magician/src/magician_v2/agents/storage.rs` |
| Agent scheduler | `magician/src/magician_v2/agents/scheduler.rs` |
| WakeUpQueue | `magician/src/magician_v2/agents/wake_up_queue.rs` |
| Definition store | `magician/src/magician_v2/agents/definition_store.rs` |
| Approval | `magician/src/magician_v2/agents/approval.rs` |
| Memory / episodes | `magician/src/magician_v2/agents/memory.rs` |
| Memory defaults | `magician/src/magician_v2/agents/memory_defaults.rs` |
| FileV2Store | `magician/src/magician_v2/storage/file.rs` |
| `TaskSchedule` | `magician/src/magician_v2/storage/task_models.rs` |
| Artifact V2 service / `start_execution` | `magician/src/magician_v2/artifact_v2/service.rs` |
| Durable artifacts | `magician/src/magician_v2/artifacts/durable_store.rs` |
| Realtime events | `magician/src/magician_v2/realtime_events.rs` |

### Frontend

| Concept | File |
|---|---|
| Agent store | `ui/unified-ui/src/lib/stores/agentStore.ts` |
| Task store | `ui/unified-ui/src/lib/stores/taskStore.ts` |
| Approval store | `ui/unified-ui/src/lib/stores/approvalStore.ts` |
| MUIJ store | `ui/unified-ui/src/lib/stores/muijStore.ts` |
| WebSocket dispatch | `ui/unified-ui/src/lib/realtime/v2-websocket.ts` |
| App routes | `ui/unified-ui/src/routes/(app)/` |
