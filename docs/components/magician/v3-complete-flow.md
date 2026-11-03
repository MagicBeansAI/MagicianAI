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
│     max_iterations            default 12000
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

**Task file workspace.** The admitted task identity supplies a dynamic shell
working-directory root. Every canonical subdirectory of that task is allowed,
including execution downloads and nested renders; sibling tasks and symlink
escapes receive no additional access. A generic shell without an explicit
working directory defaults to `<task>/workdir/`. The decision prompt names this
directory and explains how to publish generated files. It lists only the task
folders every file-writing tool accepts (the file executor's checks and the
`write_file` / `edit_file` staged writers, which take a task path only in its
workdir), judged by those tools' own code, so it follows any sandbox or layout
change. A staged write refused outside the scoped workspace names the task
workdir and the scoped workspace instead of the sandbox roots. Coding providers
retain their explicit coding workspace. Static sandbox roots remain configured.

**Tool-produced file capture.** After a pack or native shell action completes, the
executor scans structured results, copies discovered files into both
execution-scoped and task-scoped `outputs/`, registers
`tool_output_file`, and emits `ArtifactCreated`.
`is_allowed_tool_output_capture_path` (`executor.rs`) accepts the process
CWD, `std::env::temp_dir()`, `/tmp`, `/private/tmp`, and the admitted task tree.
Supported declarations include `output_path`, `output_file`, `export_path`,
`wav_path`, `mp3_path`, `manifest_path`, and `path` entries inside
`outputs`, `exports`, `files`, `images`, or `artifacts`. Relative paths resolve
against the shell working directory. Input-file fields do not publish files.
The displayed filename comes from the source basename; managed storage paths
retain execution/task identity.
The canonical output contract is `outputs: [{path, content_type}]`. Each file
can declare any valid MIME type; `mime_type` and `media_type` are also accepted.
The shared `artifact_v2/file_types.rs` resolver uses this metadata first, then
bounded byte inspection with the maintained `infer` signature library when
metadata is absent. Small complete JSON documents and readable UTF-8 text have
generic fallbacks; unknown bytes remain `application/octet-stream` and stay
downloadable. Filename extensions never determine captured output MIME types.
Signature classification does not replace producer-specific decoding checks.

**Task-user fallback.** Produced files rank ahead of inline probes in synthesis
evidence, and their metadata grounds binary file names and links. Yielded
deliverables that sit directly in the execution's outputs directory get a
server-built `task_download_url`
(`/api/magician/v3/tasks/{task}/outputs/executions/{exec}/outputs/{file}`), so
they ground "file X is attached" like tool-written files.

**Answer grounding (claim level).** `artifact_v2/answer_verification.rs` is
one core used by both the task-user publish step and the agentic loop's
terminal gate. It follows FActScore/SAFE/RAGAS-style claim verification with
deterministic figure checks.

1. **Corpus.** A `GroundingCorpus` holds the complete evidence: up to 400k
   characters per file, 4M in total and 400 artifacts, all read before any
   prompt budget trims previews.
   - Execution-bound checks (task-user answer, `notify_owner`, Relentless)
     share `build_execution_grounding_corpus`: every artifact of the execution
     (not just the 30 in the synthesis prompt) and of its delegates (three
     levels, 24 executions, a quarter of the budget; children found through
     outputs and delegation records), the children's returned results, source
     and prior outputs (agent-written), plus Magician's runtime records:
     verification-gate verdict, each delegation's agent/child/settled status,
     and change delivery. A runtime record alone does not make a run checkable.
   - **Change delivery.** `change_delivery.json` beside the execution
     (`vibedev::change_delivery::ChangeDeliveryRecord`): mode
     (`trusted_branch` / `sandboxed_fallback` / `sandboxed`), project folder,
     git state (`repository_root` / `not_a_repository` / `inside_repository` /
     `not_a_directory` / `no_git`) with reason, fallback reason, trusted
     branch. Written by dispatch/resume (`vibedev_run_trust_mode`) and by
     `run_coding_task` on the engineer's execution; read for the execution and
     every delegate. "No branch: not a git repository" is grounded only by
     `not_a_repository`; delegation instructions and a child's self-reported
     outcome are never evidence.
   - The terminal gate uses every admitted source in full.
   - JSON, including JSON nested in MCP `content[].text`, is flattened to
     `path = value` lines; a claim repeating a value gets that line as exact
     evidence (a bare `true`/`false`/`null`/`ok` or ≤ 2-char value only when
     the claim also names its key, e.g. `all_ok=true`).
   - Every artifact's payload metadata is evidence, including the captions,
     OCR text and transcripts of media. Produced files contribute name, type
     and download link.
   - The agent's own yield files (`terminal_decision:*`) are not evidence for
     its answer.
2. **Claims.** Sentences in any script (`.!?` followed by a non-lowercase
   character, and `。！？।؟`), list items, table rows, code blocks, or the
   leaves of a JSON answer.
3. **Figures.** Material figures are located deterministically:
   - Unicode digits;
   - Western, Indian, European, Swiss and NBSP grouping, where an ambiguous
     `1.590` is tried both ways;
   - rounding, percent/fraction, and lakh/crore/thousand/million/billion/
     万/亿 scales;
   - sums, differences, ratios and percentage changes of located figures;
   - line totals: a unit value times a count the claim states (`×5`, `5 ×`,
     `qty 5`), when the count sits beside the unit value in a tool result: on
     the same line (`$38 (Qty: 5)`) or as a sibling field of the same JSON
     object (an item's `price` and `quantity`). A count elsewhere in the
     result (a rating, another item's quantity) derives nothing.

   A located figure overrides an `absent` verdict only when it has four or
   more significant digits.
4. **Per-claim evidence.** Each claim gets, in this order, within 3,600
   characters:
   - up to two runtime records (change delivery, delegation, verification
     gate) sharing ≥ 2 words with the claim;
   - figure windows, with the table's column names (a recurring figure is
     shown from a result the answer cites, where the claim's label sits);
   - results the answer cites by `result_ref`, sharing ≥ 2 words with the
     claim: the two best matches plus the best of each other cited tool, up
     to three in 1,800 characters, so a claim about several sources sees each;
   - exact result values the claim repeats, with a small window, each
     key/value pair once (the same value under another key matches
     separately);
   - the top IDF-weighted lexical chunks (CJK as bigrams);
   - when the local embedding model answers, the two nearest semantic chunks.
     Vectors are cached per corpus and shared by evidence fingerprint across
     terminal re-reviews. If the model is down, retrieval is lexical only.
5. **Judge.** `task_user_claim_judge` v1.0.0, routed as
   `evidence_precision_judge`, returns `supported`, `contradicted`, `absent`
   or `not_a_claim` per claim, plus `core`.
   - Observed actions go with every batch. For the task-user answer they are
     the run-effects ledger (below); without a ledger, and for owner-text
     checks, they come from `GroundingCorpus::observed_actions`: every tool
     call of the run and its delegates, counted per tool with one redacted
     outcome line each (failures marked; recalls and catalogs counted without
     one). The in-run gate keeps its own list.
   - Batches are up to 16 claims or 56k characters, at most 64 calls, 4 in
     parallel.
   - An unreadable reply is retried once, then split down to single claims.
   - A claim never ruled on is unreviewed, unless it states a figure nobody can
     find; then it is a core failure.
6. **Policy.**
   - A claim fails when contradicted, or when absent unless its figures are
     located and distinctive.
   - A failing claim is core when the judge says so or when it states a
     figure. Figures are never removed silently.
   - Failing supporting sentences, list items and JSON fields are removed;
     table rows and code are noted.
   - The task-user finalizer records what it removed (`removed`), what it sent
     to the grounding repair (`repaired`) and what stayed unconfirmed under the
     caveat (`unconfirmed`) — each claim with the judge's reason — in
     `outputs/answer_claim_check.json` beside the answer, so "Magician removed
     N statements" can be traced. An answer with nothing removed clears it.
   - The run's settled verification gate outcomes (from its recent
     `verification.lifecycle` events, e.g. "unverified: no required checks are
     configured for this project") are evidence recorded by Magician, so an
     answer that reports them is not stripped as unconfirmed.
   - Removing more than half of an answer's claims counts as failing it.

**Run effects.** Every pause/resume segment writes its tool calls into the
same execution artifact index, and each `tool_call_evidence` record carries an
`effect` (kind, target, bytes, a result head for shell runs and failures;
`artifact_v2::run_effects`). `RunEffectsLedger` aggregates the whole index plus
delegate records: changes and reads grouped by tool, outcome and folder (up to
8 names, bytes, "N of M read back after their last change"), successful runs
(newest 12), a count of owner answers (no text) and other calls by tool.
FAILED calls are labelled and never count as done; an `exists` check is not a
read-back. Bounds: newest 2,000 records, 200 lines, 8,000 characters; older
evidence without an effect is read from its file (newest 400, 512 KB each,
8 MB total). The ledger is the task-user judge's observed actions, the protected bundle
field `run_effects` for both syntheses (`execution_output_synthesize` 1.2.0,
`task_user_output_synthesize` 1.1.0) and the grounding repair, and two corpus
entries: "RUN EFFECTS (recorded by Magician)" (tool-result origin; counts,
outcomes, bytes, heads; no path, command or time) and "RUN EFFECT PATHS"
(agent-written origin). Agent-chosen names have control characters and line
separators folded, so they cannot forge a ledger line. A claim with no
matching successful call is still `absent`. `task_user_claim_judge` stays
1.2.0 (the decision engine's `claim_support` reference hashes it), and
`answer_claim_check.json` lists each statement once per list.

**Hardening (review pass, 2026-10-03).**
- **Who can prove a figure.** Evidence has an origin. Tool results can prove
  a figure. Agent-written text cannot: the agent's tool arguments
  (`tool_call_evidence`), earlier outputs and answers (including caveated
  ones), and child results are labelled `AGENT-WRITTEN` and never prove a
  figure on their own. The same record's `result.preview` is the tool result
  the checker reads: it keeps a text result whole up to 64 KB
  (`tool_call_evidence_result_text`), not the 4 KB history line, which replaces
  a larger result with a `text_omission` marker and left claims about it (a
  coding run's summary, branch and test output) unprovable.
- **When a found figure overrides an `absent` verdict.** All of these must
  hold:
  - an exact tool-result value matched the figure's single reading at its
    stated magnitude;
  - it has four or more significant digits;
  - the window around it shares a label with the claim (column name or
    metric, or an acronym like GMV);
  - the figure does not also appear in agent-written text.
- **How evidence numbers are indexed.**
  - JSON numbers are indexed exactly.
  - Prose tokens are read per cell when comma-joined (`15892276.59,1204`, CSV).
  - Dotted runs (IPs, dates) are not indexed.
  - Negative values are also indexed by magnitude.
- **Judge protocol.**
  - Claims and evidence are fenced in nonce-tagged `<claim>`/`<evidence>`
    blocks, with every untrusted line prefixed `│` and the prompt telling the
    judge to treat it as data (`task_user_claim_judge` v1.1.0).
  - Verdicts count only for ids in their own batch, first verdict per id.
    JSON is extracted from chatty replies, and ids a reply leaves out are
    asked again.
  - Retries and splits share a per-review budget (batches + up to 24 calls).
  - Verdicts already gathered survive a later router error.
- **Policy.**
  - Headings and JSON booleans are claims.
  - An unfound figure can't be excused as `not_a_claim`.
  - The strip guard counts only substantive claims, and a lone claim is
    never stripped.
  - Unreviewed claims, partial judge outages and claims past the caps all
    mean "not verified". A run with no evidence at all holds only its figures
    to evidence.
- **Corpus.**
  - Secrets are redacted with the tool-evidence patterns.
  - Long strings are split, never cut, so a page's middle stays visible.
  - JSON paths keep their last 256 characters, and flattening stops at the
    budget.
  - Full reads are canonicalized and confined to the task's own directory.
  - Indexes are built once per corpus. Semantic vectors are cached only on
    success and keyed by model and chunk count.
- **Publish.**
  - A verified terminal projection is now claim-reviewed whenever a judge is
    configured: clean, it publishes its bytes; otherwise it takes the editable
    path.
  - A failing JSON answer is published as a code block under the caveat.
  - The caveat's reason is markdown-inert.
  - A fallback predecessor is published after stripping.
  - Spoken summaries lose any line stating a figure the published answer
    does not.
- **In-run gate and links.**
  - The terminal gate passes a draft only when every claim was reviewed.
  - A yielded file gets a download link only when its bytes hash to what was
    yielded, and the web link classifier keeps the
    `executions/<id>/outputs/` segment.

**Hardening, round 2 (2026-10-03).**
- **Who can prove a figure.** Evidence origins are `ToolResult`,
  `AgentArguments` and `AgentWritten`.
  - A tool-call record is split: its action (the agent's arguments) is
    `AgentArguments`, and its result preview is a tool result.
  - Only arguments make a matching tool figure an echo. Earlier reports
    don't, so a correct figure repeated in them is still distinctive.
  - The terminal gate adds the run's call arguments as `AgentArguments`.
- **Verdict policy.**
  - A `supported` verdict cannot pass a figure that no tool result contains.
  - `not_a_claim` on a sentence that states a figure is held to the figure
    checks.
  - A confirmed core claim keeps its failing remarks strippable instead of
    promoting them to core.
- **Figure matching.**
  - A lone comma before three digits reads primarily as the grouped integer
    (`6,835`).
  - Derivations use only strong tool figures and figures of three or more
    significant digits.
  - The override needs a label on the figure's own line or column, ignoring
    structural words and currency codes.
- **Claims.**
  - A table's first row is a header only when a separator row follows.
  - A body with no claim units is one claim.
  - Short JSON strings become `path: value` claims.
  - Claims over 1,200 characters are split, not cut.
- **Corpus.**
  - Corpus lines are 400 characters, so the judge sees every line whole.
  - Labels are whitespace-collapsed and fence-safe.
  - Content is cut to 400k characters before it is redacted.
  - A delegating parent's corpus includes its child executions' records (3
    levels, 24 executions).
  - "No evidence" means no tool results besides produced-file metadata.
- **Judge protocol.**
  - `fence_safe` is case- and space-insensitive and folds stray line
    separators.
  - Raw provider errors are logged, not shown.
  - Claims past the review caps no longer block an answer: they were still
    held to the figure checks.
  - Prompt v1.2.0: requested written content (poems, drafts, code) is the
    deliverable, not a claim.
- **In-run gate.**
  - Advisory: only failing core claims route to repair or caveat, and
    unreviewed claims are logged and left to the publish-time check.
  - The corpus build and per-claim evidence run on the blocking pool.
- **Semantic retrieval.** Failures are remembered for 60 seconds, chunk and
  claim embedding run concurrently with an 8-second timeout, and lexical
  retrieval uses an inverted index.
- **Publish.**
  - A verified deliverable too large to review whole, or whose review bundle
    cannot be built, is published as its bytes, and the gap is logged.
  - A task-user synthesis failure checks and publishes the execution's own
    answer, so there is no task error.
  - The summary sidecar keeps the "Not fully verified" caveat.
  - A stale voice line is cleared when an answer has no checked speech.

**Hardening, round 3 (2026-10-04).**
- **Figure matching.**
  - Evidence and claims split at word and cell boundaries, never inside a
    figure. JSON strings keep their own line breaks.
  - A short percentage ("up 4.2%") counts as derived when it is the change
    or share of figures stated in the same claim.
  - Code fences (any language other than data formats) are deliverables, so
    their literals aren't figures. Data fences (csv, json, tsv, none) still
    are.
  - A table header that states a figure is a claim. Unicode digits count as
    figures.
- **Echoes.**
  - A figure is an echo only if the agent sent it at or before the tool
    returned it, ordered by record time, so a report saved after a query
    doesn't taint it.
  - Only a call record's `result.preview` is a tool result.
  - Memory, ledger and recall tools are agent-written.
- **JSON answers.** Up to 5,000 leaves are claims, and leaves past that make
  the answer unreviewed. Long fields are split.
- **Judge prompt.** Labels sit behind the `│` fence, look-alike angle
  brackets and zero-width characters are folded, and closing tags carry the
  nonce.
- **Security.**
  - Credential keys (password, secret, token, api key, credential, private
    key) and `scheme://user:pass@` are redacted in evidence, including tool
    arguments.
  - Semantic retrieval only runs against a local embedding endpoint.
  - Logs carry failure counts, never claim text.
  - Removing statements always appends a note.
- **In-run gate.**
  - App-guarded runs get deterministic figure checks only.
  - The latest 64 call arguments are kept.
  - Figure failures survive a judge outage.
  - Semantic sharing is keyed by principal and workspace.
- **Publish.**
  - A synthesis failure retries first, and only then publishes the checked
    execution answer. A cancellation still fails.
  - A deliverable too large to review whole, or whose review cannot be
    prepared, is published as its preview under the caveat with a link to
    the full file.
  - The caveat applies to markdown and plain text only; HTML, XML, CSV and
    JSON are fenced. An existing caveat is stripped before review and
    re-caveating.
  - The previous voice line is removed before a new answer is written.
  - LLM task summaries that state figures absent from the answer are
    discarded.
  - A blocking-task failure degrades to unreviewed instead of re-running on
    the async worker.
  - Delegate ids are validated, and a quarter of the artifact budget is kept
    for child executions.
  - A completed run that yielded its own answer (a `partial_findings` note
    from `render_yield_partial_findings`) publishes it (summary plus declared
    open items) instead of an execution-output synthesis, without the
    opened-page coverage note; this claim check decides what the evidence
    supports. A note whose summary is the runtime's requirement status (the
    model gave no answer) carries no summary and goes to synthesis.
  - Results cited by the run's terminal notes (`result_ref_v1_…`, resolved
    through its effect records to their calls) are marked cited. A figure in
    several results is shown from a cited one, then from the line carrying
    the claim's label; figures and cited lines precede field and chunk
    evidence in the claim's budget.
  - A found figure is also shown with the rest of its result that bears on
    the claim (`figure_record_lines`, up to 8 lines): lines carrying the
    claim's distinctive words (in at most 6 of the result's lines), and for
    a list entry the same field of the list's other entries, each fact once.
    A cart total is read with what the cart holds even when its item lines
    sit far from the total.
  - Synthesis prompts split the artifact preview budget evenly across
    results (a short one passes its share on), cut long ones at both ends
    so a closing total survives, and rank the run's terminal notes first.
  - A grounding repair is told every failing claim with the judge's reason
    (`repair_feedback`, up to 12), and task-user prompt 1.2.0 makes it an
    edit: unlisted sentences stay verbatim, only a listed claim's unsupported
    detail goes, and a supported figure is never downgraded to unverified.
  - `make test-final-answer-live-eval` replays real runs' finalization with
    the live router (cases in `data/magician_v2/evals/final_answer/`). Each
    case dir keeps every claim-judge prompt and response under `judge/`
    (`MAGICIAN_CLAIM_JUDGE_DUMP_DIR`, read only by evals).

**Owner-facing text outside task answers (2026-10-04, owner decision: full
check everywhere).** `check_owner_text`
(`artifact_v2/answer_verification.rs`) runs the same claim check on owner-facing
text that does not go through the task-user finalizer.
- **What it returns.** The text unchanged when it verifies; with failing
  supporting remarks removed and noted; or under the "Not fully verified"
  caveat.
- **No tool evidence, no check.** General-knowledge replies have nothing to
  check against.
- **Review modes (`OwnerTextReview`).**
  - `Figures`: only a figure that no tool result contains marks the text.
  - `Engine`: the figure checks run in code, then the decision engine's
    `claim_support` pack (one item per claim with its evidence lines, the
    owner's request as shared context) rules on the rest. A claim it answers
    confidently is held to the same policy as an LLM verdict. A claim it
    leaves unanswered (low confidence, outage, past the operation's limits)
    keeps the figures-only rule, so an engine outage never caveats a reply.
    The whole review is capped at 8 seconds, then falls back to figures only.
  - `Llm`: the LLM claim judge (`evidence_precision_judge`) has 45 seconds,
    then the check falls back to the figure checks. A claim it could not
    review leaves the text "Not fully verified".
- **Chat inline turns (`chat/service.rs`).**
  - Each native and harness tool call's arguments and raw result are
    collected as evidence. Governed-app turns are excluded.
  - Reviewed in `Engine` mode (owner decision 2026-10-04: no LLM call on the
    reply path). The `claim_support` route in `decision-engine.yaml` is gated
    without a recorded qualification, verdict threshold 0.85, Jev primary
    and Kev-4B backup; turning the route off leaves chat replies on the
    figure checks.
  - The final reply is checked before `build_assistant_text_message`. The
    persisted message (SSE `done`, WS `ChatMessageReceived`) replaces the
    streamed draft on web, Android and iOS.
  - A reply with `<speech>` segments keeps them intact. A failing one gains
    the caveat and a spoken "couldn't fully verify" segment.
  - Delegated voice turns (`delegate_to_chat`, concurrent voice) inherit
    this.
- **Live voice (`magician-media` voice orchestrator).** An assistant
  transcript spoken by the realtime model is checked, figures only, against
  the turn's tool results in the session history before it is saved and
  shown. The audio itself cannot be recalled.
- **`notify_owner`.** A notification sent from a task run is checked against
  that execution's tool results (`load_execution_grounding_corpus`) before it
  goes to Attention and the bots, with the LLM judge (`Llm` mode).
- **Record of each check.** `notify_owner` and Relentless checks append to
  the execution's `owner_text_claim_checks.json` (newest 32): surface, user
  request id (notifications), outcome (`verified` / `supporting_removed` /
  `caveated`), and each removed or unconfirmed statement with verdict
  (`unsupported` / `contradicted`) and reason. Chat replies are not recorded.
- **Delegated work.** `load_execution_grounding_corpus` builds the Corpus
  above, so a coordinator's briefing is judged against its engineers' tool
  results.
- **Relentless.** Reports and accepted drafts are checked against the bound
  execution's tool results before publication, figures only (their content
  already passed Relentless's critics), unless the owner approved those exact
  bytes.

**Publish outcomes (task-user).**
- **Verified:** publish, with unsupported supporting claims removed.
- **Core claims fail:** one repair names them and is checked again. If it
  still fails, a predecessor that passes review is published; otherwise the
  answer goes out with a visible "Not fully verified" caveat quoting the failed
  claims. Markdown and text get a leading caveat line. HTML stays `text/html`
  with an escaped `<aside data-magician-caveat="unverified">` first in its
  body; its readable text (summary, voice) leads with the same caveat
  paragraph. JSON and other formats are fenced under the caveat.
- **Judge unavailable** (no router, router down, no evidence): the answer goes
  out with the caveat, saying its figures were checked against the tool
  results.

The generic "verified final summary is unavailable" line remains only for an
empty answer. The terminal gate maps the same result onto its verdict:
unsupported core claims with incomplete coverage become a caveat, and anything
else unsupported is repaired. A judge that cannot run at all is still an error
there.

The legacy bounded excerpt (`task_user_grounding_source_excerpt`) is kept for
its existing callers.

**Output serving.**
`GET /api/magician/v3/tasks/{task_id}/outputs/{artifact_path:.*}` with
inferred inline media types. `MAX_SELECTED_ARTIFACTS = 30`
(`artifact_v2/synthesis.rs`). Promoted media files become
`audience=user`, `role=user_media` refs on `FinalizedOutputs.media_outputs`.

**Delivered files.** Three sources, in this order, are attached to the
answer, never replacing it:

1. files the run declared in `yield.deliverables` (see `YIELD_DECISION.md`),
   whatever the summary says;
2. a file the run wrote (`tool_output_file`) and named as delivered — in its
   outcome summary, its verified `task_deliverable` / `partial_findings` /
   `summary_note` notes, or a completed micro-goal of its own durable task
   state;
3. only when 1 and 2 found nothing (a run out of budget never yields), a
   regular file in the task `workdir/` (no symlinks, hidden and dependency
   dirs skipped, repository checkouts — `.git`/`.hg`/`.svn` — skipped
   entirely, bounded walk, at most `MAX_DELIVERED_FILE_BYTES`) modified since
   the execution started whose name or absolute path the same delivery text
   gives.

Capture also records a tool result's own top-level `path` / `file` /
`artifact` / `workspace_path` as a `tool_output_file` for calls that write
(`export_result`, shell, capabilities named write/save/export) when it is a
regular file inside this task's workdir or execution outputs and the result
is not a read (no `content`/`text`/`data` field); `workspace_path` resolves
against the result's `workspace_root`.

The newest write of each name, whole-name match ignoring case, digest-checked
copy to `outputs/delivered/<execution>/<name>`, at most eight,
images/audio/video left to `user_media`, the published answer's own bytes not
attached twice. They are `audience=user`, `role=user_attachment` refs on
`FinalizedOutputs.delivered_file_outputs`, promoted before task-level
synthesis and committed with the primary output (media is registered after
it): the terminal chat card is built from that commit and lists them under
their own names, up to eight files, the primary deliverable first. The first
declared file is the primary deliverable:
`FinalizedOutputs.primary_deliverable_output_id` →
`TaskRefs.primary_deliverable_output_id` (serde-defaulted; cleared by the next
root answer that declares none). `TaskRefs.latest_delivered_output_ids` names
the latest answer's files; earlier runs' attachments stay in `outputs` but
are not linked or listed with a newer answer. The task outputs endpoint reports it as
`primary_deliverable_output_id` plus `primary: true` and a `download_url` on
the entry; chat `session_output` file blocks (`ContentBlockRecord::File`)
carry `primary: true` for it, and the chat projection always includes it.

**No host paths reach the owner.** `artifact_v2::delivered_links` is the one
rule for user-facing text: an absolute path or `file://` link into the task
directory whose file name is a delivered file becomes a link to its served URL
(`/api/magician/v3/tasks/{task_id}/outputs/delivered/<execution>/<name>`) —
matched on the whole path (workdir source, execution copy or promoted copy),
never on the file name alone; any other path into the task and every other
`file://` target is demoted to its file name (an image link to its alt
text). Only prose is rewritten: fenced code blocks are left as written, an
inline code span that is exactly one such path is replaced as a unit, and any
other code span is left alone. The task-user synthesizer's bundle carries `delivered_files` (name, title,
size, media type, served `url`, `primary`, and for a primary text
deliverable a ~4 KB `head_excerpt` plus its markdown `outline`) with the
instruction to name and link the primary deliverable without repeating its
contents or host paths; the grounding corpus holds the delivered-file facts
as Magician-recorded evidence (the excerpt as agent-written text), so the
claim check never strips those names or links. The finalizer then applies
the rule by format as the safety net: markdown/plain answers (plus a
`**Files**` list of delivered files the answer does not already link), HTML
answers (`href`/`src` values and text nodes; `<pre>`, `<code>`, `<script>`,
`<style>`, `<textarea>` and comments untouched; a dead link's attribute is
dropped), and JSON answers (string values only, keys and structure kept). A
verified-file answer (`OutputBody::File`) is published byte for byte — its
digest is re-checked on publication and byte preservation is its contract —
so only Magician's own text about it (chat summary, "Done.") is rewritten; the
chat channel applies it, with the latest answer's files only, to the terminal `TaskStatusUpdate` summary and the
"Done." message (which also lists the files). The delegate's
`chat.delegate.output_ready` payload, `get_task_details` and the awaited
`create_task` result carry `delivered_files: [{output_id, name, url, size,
primary, media_type}]`.

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

`trigger_seq` comes from the scoped scheduler entry, but is always allocated
above the highest sequence already used for the agent/goal: the runtime's
active, queued and recently completed tuples
(`AgentRuntime::highest_known_trigger_seq_in_scope`) and stored episodes
(`AgentMemoryService::highest_episode_trigger_seq`). Deleting an agent drops
its scheduler entry but not its episodes (scoped memory root), so a re-created
agent with the same id would otherwise restart at 1 and collide with tuples
that already ran. Manual triggers use `AgentScheduler::allocate_trigger_seq_above`;
a cron fire already consumed by `collect_due_cron_triggers` is moved with
`rebase_consumed_trigger_seq`, which keeps `requeue_due_cron_trigger` matching.

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

Planning resume receipts use canonical JSON and exact float parsing, so map
ordering or lost numeric precision cannot invalidate an answer. A fresh answer may supersede an
unverifiable, unapplied legacy receipt for the same paused state and question;
its payload is never replayed. Committed receipts retain strict verification.
Resumed proposals remain planning-only until the separate approved task launch;
answered questions leave `WaitingUser` before the resumed plan is published,
and resume polls use stack headroom for deep planning continuations. The durable
planning-recovery monitor awaits each claim, resume and planning run off its own
frame, and planning's per-parameter classification boxes each router call, so a
recovered plan reaches the LLM router within the 2 MiB worker stack (the unboxed
chain overflowed it and crash-looped the service on restart). Terminal
tasks (including archived ones) release pending questions and, when the task
never started a run or was cancelled or archived, a plan's pending approval
(the plan fails with "Task ended before the plan was approved" and its
`plan_approval` ask resolves as cancelled; a run that ended beside a re-drafted
plan keeps it, so the run's own outcome stays on the card), and cannot claim or renew
planning recovery.
Full and partial replans carry the accepted answer and its question into the
planning query, so rebuilding slots preserves the owner's resolved decision.

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

### 10.3 Delegation across a restart

A restart while a parent sits in `WaitingChildren` must end in exactly one of
two places: the child resumes, or the child fails with a reason the parent can
read. Never a hang.

- **Interrupted child.** Startup re-admits it through
  `recover_interrupted_delegated_child_execution`, which re-checks every
  integrity binding (definition digests, waiting state, lineage, routing
  seal). A `DispatchFailed` there is deterministic — the same durable inputs
  fail at every boot — so the child is force-failed with
  `delegated child could not be recovered after a restart: <reason>` and its
  outcome persisted; the parent's `ChildCompleted` then reconciles
  immediately. Only transient errors (`Runtime`, `ServiceUnavailable`) stay
  deferred. It used to log and drop every failure, leaving the child
  `running` until the parent's durable deadline retired the tree.
- **A deferral is re-checked without a restart.** A transient child failure
  (e.g. `stateless_preseed_generation_still_within_visibility_grace`), and a
  boot-sweep deferral inside the pre-seed visibility grace
  (`PRESEED_VISIBILITY_GRACE_MS`, 2 min), go to the persistent recovery
  monitor (`schedule_deferred_interrupted_runtime_recheck`), which re-runs
  `recover_interrupted_runtime_execution` once the grace has passed and
  retries with backoff until the run is parked or terminal and projected.
  Before, the parent sat `WaitingChildren` until the next restart.
- **A run started after restart gets its event scope back.** The canonical
  event scope registry is process-local; `execute_agentic_direct_with_outcome_inner`
  restores a missing one from the task (`restore_runtime_canonical_scope`), so
  a recovered child's or resumed parent's events — its diff review included —
  are recorded rather than journalled transport-only.
- **Routing is compared by endpoint.** `parent_engine` is not sealed
  authority: it is re-derived from the run's pin or the current process
  engine on every load. Recovery and `persist_execution_llm_routing_overrides`
  both compare with it stripped, so a restart under a different engine — or a
  sidecar sealed before launches were stripped of the parent — is not a
  routing mismatch. A different endpoint still is.
- **Title and waiting state.** The binding seals the delegation's context,
  while the child row is titled `[Delegation] <context>`; recovery compares
  the title without that prefix (`delegated_child_title_matches`). Before,
  every ordinary child (a VibeDev coding child among them) failed recovery
  with `delegated child recovery runtime binding mismatch`. An interrupted
  child may be the launch-ready `Runnable` shell or a child that was
  mid-segment (`Executing`) when the process died
  (`DelegatedChildRecoveryAdmission::admits_waiting_state`); the exact
  generation is still fenced by `bind_interrupted_runtime_recovery_overrides`,
  which defers a child whose segment is still live.
- **A dead process owns nothing.** A run's worker holds a phase lease (5 min)
  and a placement pin (`PIN_TTL_MS`, 15 min) that every commit renews. After a
  crash or restart both still sat unexpired on disk, so recovery treated the
  run as live (`BaseExecutionRecoveryAuthority::Live`) and a VibeDev run or
  crew lane showed `running` for up to fifteen minutes. A lease or pin whose
  holder is an `inproc-{pid}-…` worker of a pid that is not this process and no
  longer exists (`WorkerId::process_has_exited`) now binds nothing: the runnable
  scan, `durable_owner_until` and `claimable_by` ignore it, and the next claim
  takes it over at the next fence. Any other holder, this process's pid, or a
  live pid keeps the old expiry. The same holds for a recovery admission
  (`preseed.admission`, renewed for `PIN_TTL_MS`): it records the claiming
  process (`holder_pid`, sealed under its own integrity domain so older
  admissions hash as before), and once that process has exited it excludes
  nobody (`admission_binds`) — a crash during recovery no longer holds the run
  for fifteen minutes. An admission without `holder_pid` keeps its expiry.
- **A recovered child has stack headroom.** `recover_delegated_child_from_runtime`
  runs the recovered loop under `with_stack_headroom`, as a launched child gets
  from `spawn_with_execution_token_meter_in_set`; the first recovered coding
  child otherwise overflowed its 2 MiB execution worker as its loop started.
- **The resume note is not an identity change.** An exact recovery resumes
  under the committed goal plus the note `build_exact_resumed_goal` appends
  (`[Prior actions in this execution (checkpointed at iteration …)]` or
  `[Execution resumed from its exact checkpoint …]`), and the run identity's
  `primitive_goal` mirrors `ctx.goal`. `complete_run_identity_matches` (the
  exact-continuation check and `adopt_owner`) accepts a live goal that is the
  committed goal plus exactly one such note; every other field, and any other
  goal change, must still match. Before, every run recovered from a checkpoint
  failed with `exact continuation owner/profile composition … does not match
  committed identity` and was marked failed.
- **Unrecoverable parent.** A `ChildCompleted` reconcile that fails on the
  parent's own delegation record
  (`is_permanent_delegation_reconcile_error`: a partial stateless address or
  an invalid/mismatched children-handoff descriptor) cancels the parent tree
  and acknowledges the wake, instead of retaining the claim and retrying every
  lease forever.

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
  → file sandbox (mode, allowed_roots, read_only_roots, delete_policy)
  → capability schema validation (strict on load)
  → trust level + ApprovalRule evaluation (agent path)
  → allowed | NeedsApproval | blocked
```

### 12.1 Shell and file sandbox

Configured in `magician-config.yaml`, enforced in native executor paths
before filesystem/shell operations.

`execution.file_sandbox` has two kinds of configured root, both resolved
like the paths checked against them (relative to the process directory,
symlinks followed, a dangling link resolved to its target):

- `allowed_roots`: agents may read and write.
- `read_only_roots`: agents may read, check existence and list, and copy
  from. Every mutation through the file tools is refused: write, append,
  delete, create_dir, a copy's destination, and either end of a move.
  Sanctioned writers (`create_note`, `append_note`,
  `propose_program_revision`, the missions tools) are not file tools.

A path is inside a root when it starts with it, or when one of its existing
ancestors is the root's own directory (same device and inode), so one folder
spelled two ways — `~/MagicianNotes` links to `/Volumes/build/…` while the
case-insensitive volume mounts as `/Volumes/SSD1` — is not "outside". The
staged writers' workspace and task-workdir checks use the same rule. Inside a task,
`write_file` and `edit_file` anchor a relative path at the task workdir, as
`read_file`, the `files` tool and the shell do, so a relative write lands where
a relative read looks instead of being staged under the scoped workspace.

When a path sits under both kinds, the deepest configured root decides and a
tie goes to read-only. So a scope's `programs/` listed read-only stays
read-only under the writable scopes tree, and `<notes space>/Workspaces`
listed writable stays writable inside a read-only notes space. Run-time
roots (`extra_writable_roots`: the task workdir, folders the owner approved
this session, `delegation_files` folders) admit reads and writes only where
no read-only root applies; they never outrank one.

A mutation is checked at the target a write lands on and, for a delete or
move, at the directory entry itself (a symlink inside a read-only root
cannot be removed). A subtree operation (a recursive delete, either
end of a move, even onto a folder that does not exist yet) is refused when
any read-only root lies inside it; the runtime-store fence likewise refuses one that would
take a scope's transaction, proposal or pause records with it.

`native_executors::validate_file_action` applies this to every native file
action (`files`, `read_file`, binary readers and writers through
`validate_file_read_path` / `validate_file_write_path`). The staged writers
(`write_file`, `edit_file`, `apply_patch`) refuse a path under a read-only
root before they stage a diff (`staged_file_edit::resolve_write_path`), and
the diff-approval apply (`file_edit::snapshot::apply_transaction`) checks
every final target again against the boot sandbox the orchestrator
registers (`native_executors::register_agent_file_sandbox`), so relative
paths and multi-file patches are covered. A mutation under a read-only root
is a hard error, `<path> is read-only for agents (read_only_roots) (writable:
<roots>; read-only: <roots>)`, and never asks the owner to widen the
sandbox; a staged writer reports it as ``Refused `file_path`: …``. A path
that cannot be checked is refused. A path outside every root stays the
recoverable `PathAccessDenied`, and its message lists the roots: `path
outside allowed file sandbox roots: <path> (writable: <roots>; read-only:
<roots>)`. `unrestricted` mode skips both lists; god mode may still allow a
refused action per owner approval, but only once: a write under a
read-only root or to a hard-linked file never offers a session or always
grant. An in-place change (write, append, copy onto, an approved Modify) of
an existing file with more than one hard link is refused when read-only
roots are configured, because another of its names may be read-only. The task-workdir shortcut of `write_file` /
`edit_file` resolves symlinks the same way, checks the read-only roots, and
creates new files with `create_new`, so a planted link cannot carry a write
out of the workdir; an approved Create does the same.

The shell sandbox is separate: its OS box writes only under
`allowed_working_dirs`. It also honours `read_only_roots`: no writes and no
hard links out of them (`ln programs/plan.md Workspaces/plan.md` would give
a program a writable second name; SBPL `file-link`, a separate read-only
bwrap mount on Linux), with a writable root inside one re-opened. VibeDev
engines inherit it in their box, and the coding profile (coding-run spawns,
`box_harness: false`, and the god-mode override's unboxed shell inside a
coding run) layers it the same way: the read-only roots after the write
allows, the run's writable roots inside them re-opened (its write
allow-list, or without one the agents' allowed roots, the shell's working
dirs and the task root), all before the live-fence and real-repo denies,
and again inside the live fence's storage base after its carve-out (the
read-only parts only: the base itself is never re-opened as a whole). A
god-mode command the owner approves runs outside the shell box, and super
god mode launches with no fence. On Linux a read-only root missing when a
command starts is not mounted (`--ro-bind-try`), so one created during the
command is fenced only from the next one.

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

Successful task-workdir file writes and edits declare artifact paths for the existing capture pipeline; editable source paths remain distinct from immutable execution copies. Structured tool results include verified `magician_output_files` download references for the model to copy.

Historical planning-usage backfill reads governed Parquet files in bounded groups, with resource limits and deadlines; incomplete scans retain their retry marker. Verified checksums are reused only while file identity, length, mtime and ctime remain unchanged.
