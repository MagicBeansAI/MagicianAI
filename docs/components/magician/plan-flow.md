# Plan flow — how the planner finds tools

How Magician's planning tier decides what to do, where its tool catalog comes
from, and what happens to a plan once it exists.

**Status:** `planning.analysis_path: new` is live, run activity is attributed to plan steps
(`step.started` / `step.skipped`), and the legacy V2 planning path is removed.

## Strategies

Two exist (`strategy/types.rs::StrategyType`):

- `GuidedSearch`: beam search over tool matches.
- `AtomicComposition`: one reasoning-model call that emits an executable plan
  directly, with no matching phase.

`magician-config.yaml` sets `consumer_mode: true`, which forces
`AtomicComposition` (`v2_orchestrator.rs`, the consumer-mode guard before
`execute_strategy_with_retry`) and blocks escalation or simplification to
`GuidedSearch`. In this deployment `AtomicComposition` is the only planner.

## Where the tool catalog comes from

```
AtomicCompositionStrategy::explore            strategy/atomic_composition.rs
  → StrategyContext::merged_agent_tools()     strategy/types.rs
  → planner_agent_catalog[SelfAgent].tools    ← the resolved surface
  → group_atomic_tools                        strategy/atomic_filter.rs
  → format_for_prompt_compact                 + render_planner_tool_catalog
                                              + render_available_procedure_skills
  → validate_plan                             strategy/plan_validator.rs
```

`V2Orchestrator::visible_tools_for_definition` resolves one agent's surface:

1. **The agent's allowlist.** `ToolCatalog::agent_filtered_tools(definition.tools,
   definition.excluded_tools, ctx)`. An empty `tools` list means no
   restriction; a non-empty one matches on tool `name` or `categories`.
2. **Harness tools**: injected for `Personal` agents that declare `harness`,
   stripped for everyone else.
3. **`denied_tools`**: removed.

`build_planner_agent_catalog_for_scope` then unions the universal backend packs
onto the **owner's** entry (`merge_universal_backend_packs`), re-applies deny,
and re-sorts. `StrategyContext::merged_agent_tools` reads that entry, merges
delegate catalogs on top, and applies `denied_tools` once more. That last pass
matters: denied tools must never appear in plan steps, and a delegate exposing
a denied name would otherwise bring it back.

> **The union is planner-only, and must stay that way.**
> `visible_tools_for_definition` also feeds
> `resolve_merged_agent_tools_for_scope` → `OwnerExecutionProfile` →
> `AgenticContext::merged_agent_tools`, which the executor reads as "what the
> YAML granted" **before** injecting the substrate. `build_catalog_context`
> derives `granted_capability_names` from it, and `coordinator_grant_gate`
> decides delegate-only status by whether the agent holds `shell` /
> `read_file` / `write_file` / `edit_file` / `grep` / `glob` / `http`. A union
> upstream would make `holds_work_tool` always true, so no agent would ever be
> delegate-only and the coordinator tool gate would never fire.
> `derive_allowed_action_types` reads the same list via `has_direct_pack_tools`.
> The planner catalog is read only for prompt rendering and
> `providing_agent_id_for_tool`, and is never persisted.

Underneath, `agent_filtered_tools` reaches `LocalToolServices` in
`magician-bin`. A scope's surface is the tool-runtime registry (empty by
default: `tool-runtime-config.yaml` has `registry.paths: []`) unioned with
`scope_pack_surface`, i.e. `load_pack_defs_for_scope(principal, workspace)`
pruned by `prune_unexecutable_pack_defs` + `prune_runtime_disabled_pack_defs`.
The executor's `ScopedCapabilitySnapshot` starts from the same loader and
prunes, so plan time and run time agree on what a scope contains.

That agreement needs the planner to know the scope. The pipeline planner
(`MagicianPlannerBackend` → `plan_from_analysis`) is built with the run's
`ExecutionContext` (`with_execution_context`) and passes its principal and
workspace to the tool filter, delegate catalog, agent catalog and procedure
skills, and the context to `create_strategy_context`, which requires one. A
backend built without a context plans in the reserved `system/system` bucket
and warns. (It once fell back to `ExecutionContext::default()`
(`system/default`), which the router stamped as an explicit scope, minting a
phantom `scopes/system/default` tenant.)

A pack def comes from `skills/<skill>/tool_schema.yaml` or a
`runtime_contract:` block in `skills/<skill>/SKILL.md`; embedded compiled defs
(`execution/embedded_pack_defs/`) fill any name no disk source provides.

## The universal backend packs

`UNIVERSAL_BACKEND_PACKS` (`execution/agentic/native_integration.rs`) is the
substrate every agent gets at run time regardless of its YAML: `shell`,
`files`, `http`, `read_file` / `write_file` / `edit_file`, `glob`, `grep`,
`web_search` / `web_fetch`, `content_search` / `content_read`, the memory and
read-only introspection tools, non-face-coupled task management,
`activate_skill` / `deactivate_skill`, `tool_search`, `time_math`.

Agent definitions deliberately omit these, so the planner must union them in or
its catalog misses every primitive the executor holds.
`merge_universal_backend_packs` does so, mirroring every gate
`build_catalog_context` applies:

| gate | effect |
|---|---|
| `trust_level` is untrusted | only `SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED` |
| `discoverability == SurfaceOnly` | no substrate at all |
| catalog carries `app_commit_mutations` | no substrate — app-workflow runs dispatch under the installation's grant |
| `excluded_tools` / `denied_tools` | stripped |

**Delegates are excluded.** A delegate gets the substrate when *it* runs, but
listing it per delegate would dominate the prompt for an owner with
`delegation_targets: ['*']`, so `render_planner_tool_catalog` adds a one-line
note in each delegate section instead.

**A delegate never displaces the substrate.** `merged_agent_tools` normally
lets a delegate replace an owner's same-named tool, but not a universal pack:
the owner runs those natively. Delegates do list universal names (e.g.
`vc-researcher` → `read_file`), so without this guard a trivial file read would
carry `Providing agent: vc-researcher`. The executor is the authority here:
`extract_direct_capabilities` drops every delegate-owned tool and re-adds the
substrate from the embedded defs as the owner's direct capabilities.
`is_universal_backend_pack` is the shared predicate.

`native_integration.rs::universal_backend_pack_contract_has_no_schema_or_safe_subset_drift`
pins that every universal name has an embedded def and that the untrusted
subset stays a subset.

## Prompt shape and cost

`render_atomic_tool_prompt` renders the catalog per phase. The **outline** call
gets names and descriptions only. The **expansion** call also gets parameter
signatures, because it fills a `parameters` object per step and
`validate_plan` books every unfilled required parameter as an
`UnresolvedInput`. Signatures raise the flat listing from ~2.2k to ~12.5k
tokens, and expansion already retries up to three times, so the outline does
not get them.

Both phases also carry the agent-grouped catalog, which re-lists tools with
their owning agent. That duplication tells the planner which delegate provides
what, and makes the catalog the dominant term in the prompt.

## Procedure playbooks

A procedure skill has no `runtime_contract:`, so it produces no pack and never
appears in a tool catalog. Its only entry point is `activate_skill`, whose
`name` is free-form. The executor
(`AgenticContext::available_procedure_skills`) and the planner
(`StrategyContext::available_procedure_skills`) both enumerate them from
`skills::agent_procedure_skill_catalog`, keyed on the agent's `tools:`
allowlist. Skill-only entries are **not** pushed into the tool catalog.

## What happens to a plan

**Plans are advisory prose, by design.** Neither path runs the plan as a step
program. V3 Run (`POST /v3/tasks/{id}/execute`) uses
`ArtifactV2Service::approved_plan_runtime_context` (see "What Run does with an
approved plan (V3)" below); legacy V2 conversation executions use
`V2Orchestrator::execute_plan_graph`. Both render the plan through
`render_plan_graph_runtime_context` (goal, success criteria, objectives,
constraints, a numbered `Planned Steps` list with `Tool:` and
`Providing agent:` per step) and hand it to the agentic loop as runtime
context, prefixed with:

> Use this approved PlanGraph as advisory execution context. Follow it when it
> matches runtime observations; adapt when the live environment proves it
> stale or incomplete.

The executor then selects tools from its own `ScopedCapabilitySnapshot`.

`execution/lowering.rs` still holds the `PlanGraph → ExecutableStep`
translation (`lower_plan_to_executable_steps*`,
`lower_step_to_executable_action`), but **every caller is a test**. It is
kept deliberately, not wired; do not read it as evidence that plans execute as
step programs.

Because plans are advisory, a planner/executor catalog mismatch does not fail
loudly: the plan names tools the executor may not hold, or omits ones it does.
The parity tests in `strategy/types.rs` guard this.

Parameter priorities (outline parameters and step slots) are classified in one
batch per plan by `strategy/parameter_priority_decisions.rs`: the decision
engine's `plan_parameter_priority` operation, with the planner's
`parameter_extraction` LLM as shadow reference and fallback for unresolved
parameters (bounded concurrency, safe defaults when neither answers). An
omitted decision field, or a failed fallback, keeps the caller's existing
value.

## Before the outline: one extraction, one rewrite

A planning turn runs intent-classifier (`query_analysis`) → slot-extractor →
elicitor → query-rewriter → planner. Each LLM step runs once per turn
(`docs/archive/plans/2026-10-01-planning-latency.md`):

- The slot-extractor writes this run's `SlotGraph` (LLM extraction plus
  memory-resolved slots). The elicitor passes those slots to
  `ElicitationService::elicit_and_rewrite_with_scope(.., extracted_slots, ..)`
  (`pipeline/system_agents/elicitor.rs::slots_extracted_this_run`) instead of
  extracting again. A graph from an earlier run or another producer still
  triggers a fresh extraction; a stored slots checkpoint still wins. Only
  slots in the workflow's namespace (`<workflow_id>::…`) are reused or
  replayed from a checkpoint (`ElicitationService::retain_workflow_slots`):
  the memory-resolved `entity_*` / `environment_*` slots are pipeline context
  the slot-graph repository cannot persist. 0.7.92 reused them, failed every
  planning turn on `failed to persist slot entity_…` and checkpointed them, so
  durable recovery retried the same failure every ~78 s.
- The elicitor's rewrite becomes the run's `ClarifiedTask` when it resolved
  without questions and the query-rewriter would see the same inputs (no prior
  `ClarifiedTask`, no `InterpretedAnswer`, no newer `SlotGraph`;
  `query_rewriter.rs::reusable_elicitation_rewrite`). Resume and replan turns
  still rewrite in that stage.
- A task plan waits on the owner only for a question the ask loop actually
  created. When every upfront ask is declined (for example "ask not
  recommended"), task planning stays in Planning and finishes as a draft for
  approval that carries the unresolved inputs. Entering WaitingUser with no
  question made a clarification resume fail `ReadyToExecute` and replan every
  ~2 minutes.
- `QuestionRewriter` runs its Conservative and Aggressive passes concurrently
  and scores them in that order (Aggressive replaces Conservative only with a
  strictly higher score). Both win often, so both stay.

### Analysis path (`planning.analysis_path`)

Which path produces the intent-classifier analysis
(`query_analysis/analysis_path.rs`, live-reloadable, default `legacy`):

- `legacy`: the unified query-analysis LLM call every turn, with gated
  `query_intent` engine answers applied over its intent, task clarity and
  complexity bucket.
- `new`: the gated engine decides while the LLM call starts beside it. When the
  engine answers all three heads eligibly and the message is not a slot answer,
  the LLM call is dropped and the analysis is built from the engine
  (`extracted_entities.source: deferred`); otherwise the LLM result is used, so
  it is never slower than legacy. The slot-extractor then extracts with
  `slot_extraction` v1.1.0 (no seed; slots plus `extraction_confidence` /
  `extraction_reasoning`) and writes `extracted_entities` back into the turn's
  `QueryAnalysis` artifact, so elicitation's 0.7 clarification gate reads the
  slot extractor's assessment. Elicitation skips the gate for an analysis
  still marked `deferred`.
- `shadow`: legacy, plus per-turn comparison records (analysis trace; assessed
  extraction run off the hot path) in
  `<scope>/planning/analysis_path_shadow.jsonl`.

Field map, design and eval: `docs/archive/plans/2026-09-30-planning-decisions-on-decision-engine.md` (archived; Phase C steps 3–4).

## Planning progress on the task

Planning runs on its `planexec_` execution before the task has a run, and its
events carry no task id. `progress_channel_seam/normalize.rs::planning_milestone`
maps them onto the task's progress channel (`task:{task_id}`) as
`execution.progress` notifications: query analysis, slot extraction, the
clarified task, outline started and done, expansion, plan validation
(`validating` / `valid` / `retrying`, the last a warning), `PipelineFailed` (a
warning naming the reason; durable recovery then retries), and every planning
`LLMResponseReceived` (operation, model, latency, tokens; metadata carries
`latency_ms`, tokens and `cost_usd`). A run's own LLM calls stay in its event
log. Chat and the Activity feed drop `execution.progress`. The events are
durable in the task progress log; `/execution-panel` returns them oldest first
as `debug.planning_timeline`, read 400 rows deep so a run cannot push them out.
The task panel shows them in the Plan act.

**Lineage on planning model calls.** Planning calls read `task_id`,
`agent_id`, `chat_session_id` and `root_execution_id` from the planning run's
`ExecutionContext.metadata`. `run_task_planning` sets the first three.
`prepare_execution_and_turn` and `resume_workflow_inner` fill `task_id` and
`root_execution_id` from the durable execution record
(`fill_execution_lineage_metadata`) without overwriting a caller's value.
Ledger rows from before 2026-10-01 have planning calls with
`execution_id = planexec_…` and no `task_id`.

**Backfill of older tasks.** `progress_channel_seam/planning_usage_backfill.rs`
runs once at boot, in-process, because the server owns the progress-shard
write lease.

- It finds each `tasks/<task>/executions/planexec_*` and sums its calls from
  the ledger's `llm_calls` parquet, reading only the UTC start day and the day
  after, directly with DuckDB (the governed analytics reader's 10 s cap is too
  short for weeks of partitions).
- A call has several ledger rows (legacy, started, completed, compacted), so
  each `llm_call_id` counts once with its largest token and cost values; raw
  sums overstate cost two to three times.
- It appends one `llm` planning milestone per execution, with `llm_calls`,
  `backfilled: true` and the id `planning-usage-backfill:<execution>`.
- An execution whose shard tail already has an `llm` row is skipped.
- A per-scope marker (`progress_channels/events/.planning_usage_backfill_v1.json`)
  records handled executions, so a restart does not re-scan them. One with no
  ledger rows stays unmarked for two days (`SETTLED_AFTER_MS`) in case the
  ledger catches up, then is marked done. A scope that fails is logged and
  retried on the next start.

## Planning in the chat thread

`chat/planning_listener.rs` posts the task's planning lifecycle to its chat
thread as `task_status_update` messages: `planning` (started, progress, a
clarification asked or answered), `draft` ("Plan ready for review."),
`failed`, and, when a `plan_approval` HITL resolves as `responded`, `ready`
("Plan approved — not started yet.") or `rejected`. Approval does not start a
run.

**Which session:** planning events carry the task's creating chat session
(`TaskManifest.chat_session_id`, as `chat_session_id` on `V3Planning*`). The
listener posts there first, where the run's status updates also go, and the
chat store promotes the planning card (`…::execution:none`) into the run's
card: one card for the task's life. The HITL arms (clarification answered,
plan approved) carry no session, so the listener reuses the one it learned for
the task (a bounded per-task cache). Without a known session, or for one
cleared or outside the scope, it falls back to the thread lookup, which could
pick another active session or `general` and split the card.

## What Run does with an approved plan (V3)

`POST /v3/tasks/{id}/execute` → `ArtifactV2Service::start_execution_with_launch_options`
→ `approved_plan_runtime_context`. It loads the task's approved plan (status
`Approved` with a `plan_graph`) and renders it with `strategy/plan.rs` as an
advisory **"## Preplanned Runtime Context"** block listing:

- the goal, success criteria and clarified task;
- the planned steps, each with tool, agent, dependencies, outputs and
  parameters;
- the dependencies between steps;
- the unresolved inputs.

`goal_with_runtime_plan_context` appends the block to the task goal, which
becomes the loop's `goal` prompt variable; the terminal step's success
criterion becomes `success_criteria`. The run is not driven step by step, but
it is **tracked** against the plan
(`docs/archive/plans/2026-10-01-plan-step-progress.md`):

- The agent reports `step_started` when a tool call begins a planned step, then
  `step_completed`, `step_failed` or `step_skipped` (with a `step_note` when it
  departs), using the plan's step IDs (since prompt `agentic_decision` v1.3.10,
  pinned at 1.3.12 now, plus a line in the plan block).
- Before a decision's action runs (`apply::gate` →
  `executor.rs::emit_plan_step_boundary_before_dispatch`) the runtime opens the
  step the action works on (the step it completes or fails, else the one it
  starts) and closes a step it skips. `AgenticStepStarted` and
  `AgenticStepSkipped` are canonical (`step.started`, `step.skipped`), so step
  boundaries are in the durable event log. `LoopProtectiveState.current_plan_step`
  is the current step.
- Completions and failures land in `ExecutionState`; skips and notes land as
  `plan.step.finished` on the task progress log.
- `/execution-panel` returns the plan's steps with those statuses and notes in
  `debug.selected_execution.step_statuses`.
- **Off-plan attribution.** The panel adapter
  (`magician-learning/src/execution_panel/plan_attribution.rs`) reads the log
  in order: activity inside a step's `step.started` … outcome window is that
  step's; activity outside every window is off-plan.
  `run.activity_log[].metadata` and `debug.timeline[]` carry `plan_step_id` or
  `off_plan: true`; each `step_statuses` entry carries `activity_count`;
  `debug.selected_execution.off_plan_activity_count` counts the rest. Lifecycle
  rows carry neither; a delegated child's rows take the parent's step. Nothing
  is attributed for a run with no approved plan, a log with a step outcome but
  no `step.started`, or a finished run that reported no step.

The schedule still reports one `step_execute_task`. With no approved plan or no
`plan_graph`, the run starts from the task goal alone and logs a warning. The
V2 `execute_plan_graph` path renders the same block for legacy executions.

## Run start after approval

`seed_direct_execution_context` seeds prompt memory before the first decision.
It scores the hybrid index under a 3 s budget, renders the user / agent /
agent_goal tiers concurrently, and stops at a 6 s deadline
(`RUN_START_MEMORY_SEED_DEADLINE`). Finished tiers are attached; a tier still
running is dropped with a `warn!` and the generation-zero checkpoint stays
open, so the executor's bounded checkpoint retrieval
(`agent_surface_runtime.context_retrieval.autonomous_task.deadline_ms`) fills
the missing tiers and keeps the attached ones.

## Planned runs from `POST /api/magician/v2/executions`

`skip_planning=false` with an `initial_message` creates an approved V3 task
(title from `title` or the message, description = message;
`debug`/`internal` → `Internal` lifecycle) and starts its plan in-process
(`start_task_planning`, as `POST /v3/tasks/{id}/plan`). Response `202`:
`{execution_id: <planexec_ id>, execution: <runtime row or null>,
initial_message_enqueued: true, skip_planning: false, task_id, plan,
planning_execution_id}`. An `engagement_id` with planning is refused (`400
engagement_planning_unsupported`); V3 planning carries no engagement yet.
`skip_planning=true` is unchanged.

The legacy V2 planning entries are gone:
`POST /v2/executions/{id}/message` with `skip_planning=false` returns `400
legacy_planning_retired` (the body names the V3 routes), and an accepted HTTP
launch is always direct (`AcceptedRuntimeLaunchRequest` has no mode).
`process_with_strategy` stays for agent goal cycles (scheduled goals, program
missions), and recovery still drains a `Planning` launch sealed before 0.7.92,
so the "never wait silently" branches stay for those runs.

## Failure behaviour

`validate_plan` rejects a plan that references a tool outside its catalog,
cycles, or dangling edges; `compose_with_retry` then retries the expansion
(not the outline or parameter priorities) up to three times with corrective
feedback. Because `AtomicComposition` is terminal under consumer mode,
exhausting those retries returns an empty exploration result:
`trigger_execution` finds no PlanGraph and falls back to
`trigger_execution_direct` with `skip_planning`. The task still completes on
the agentic loop, so this is logged at `warn!` naming the blocked fallback; a
degraded planning tier must stay visible.

`validate_plan` also enforces browser session/observation metadata and a
shell/file rationale. These gates read `composition_category`, falling back to
`categories`, so a missing optional field cannot switch validation off.

## Known gaps

- **MCP tools** reach the runtime through
  `execution/primitive_dispatch/governed_mcp.rs` and are planner-visible only
  when a scope skill publishes them as a pack.
- **App computed capabilities** are planner-invisible on purpose: the
  fail-closed app-origin gate in `scoped_capability_resolver.rs` requires them
  to go through the app-workflow path.
- **`keywords` and `use_cases`** on `ToolInfo` are always empty in production
  (`LocalToolServices::tool_to_info` hardcodes them), so
  `AtomicToolSet::format_for_prompt`'s sections for them are inert. That
  formatter has no production caller; the live path is
  `format_for_prompt_compact`.
