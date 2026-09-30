# True Agents: Runtime Contract

> **Code**: `magician/src/magician_v2/agents/`
> **HTTP**: `magician-api/src/web_api.rs`, `magician-api/src/agent_updates_api.rs`

As-built contract for persistent agents: YAML `AgentDefinition`, scoped `agent_runtime` storage, `AgentScheduler` + `Task.schedule`, `WakeUpQueue`, cycle admission, memory tiers, approvals, proposals, trust, and `agent.update` events.

Goals and triggers do not live on the agent YAML. There is no `run_cycle`, `AgentIdleService`, `IdleBehavior`, `WorkflowInterpreter`, `WorkflowDefinition`, `/api/magician/v2/workflows/*`, `AgentKind::Autonomous`, or meta-agent template. Workflows are out of runtime scope. Downloadable agent packs, gated creativity and browser overlays are archive/future material, not this contract.

---

## Scope

An **agent** is the durable executor of work: identity, persona, tool ceiling, trust, memory, invocation policy. A **task** is the unit of work assigned to an agent. Conversation context lives on the task and its executions (`ExecutionRun`). The user-facing chat grouping is `ui_thread_id` (default `"general"`); `V2Thread` is gone.

| Concern | Primary files (`agents/`) |
|---------|----------------|
| Definition types + validation | `types.rs` |
| Scoped YAML store | `definition_store.rs`, `storage.rs` |
| In-process registry + cycle admission | `runtime.rs` |
| Cron / event / idle registrations | `scheduler.rs` |
| Durable timers | `wake_up_queue.rs` |
| Autonomous goal text | `autonomous_goal.rs` |
| Memory I/O, tiers, consolidation | `memory.rs`, `memory_defaults.rs`, `memory_tier_interpreter.rs`, `memory_consolidator.rs`; types re-exported from `magician-vector-index` |
| Approvals | `approval.rs`, `approval_service.rs`, `approval_store.rs`, `state_machine.rs` |
| Definition proposals | `proposals.rs` |
| Trust policies | `trust.rs`, `builtin/trust_policies.default.yaml` |
| Operator feed | `agent_update.rs`, `agent_update_emitter.rs`, `agent_update_journal.rs` |
| Outward send gate | `outward_gate.rs`, `outward_actions.rs`, `outward_receipts.rs` |

The observe-decide-execute loop is the agentic executor (`execution/agentic/executor.rs`). Agents admit a cycle and hand it a V3 task + execution shell; they do not reimplement the inner loop.

Other files: `condition.rs` (prompt-section conditions), `task_ownership.rs`, `personal_agent_retrieval.rs` (app personal-agent retrieval authority), `project_knowledge.rs`, `app_memory_ingress.rs` (sealed app-memory receipts), `events.rs` (`emit_agent_cycle_started` / `completed` / `triggered`). `memory_candidates`, `memory_index`, `memory_temperature`, `memory_tier_health`, `retrieval_scope`, `memory_tiers` are facades over `magician-vector-index`. There is no `delegation.rs`, `notifications.rs`, or `workflow_interpreter.rs`.

---

## Concept model (as-built)

```
AGENT (who)
├── kind: Personal | Worker
├── tools / excluded_tools / denied_tools / denied_tool_params
├── browser_transports, trust_level
├── memory_tiers + memory_consolidation
├── invocation_policy, delegation_targets, chat_inline
├── optional autonomous_config       — Personal only
└── no goals[], no triggers[]

TASK (what)
├── agent_id (compulsory)
├── schedule: Option<TaskSchedule>   — cron / interval / once / event
├── created_by: User | Agent | Delegation
└── Artifact V3 task + execution rows
```

Scheduling is `Task.schedule` (`TaskSchedule` in `storage/task_models.rs`). Runtime cycles still carry a `goal_id` string (focus-area slug, scheduler key, or caller-supplied id); it is not a YAML `goals[]` entry.

Where [UNIFIED_AGENTIC_ARCHITECTURE.md](./UNIFIED_AGENTIC_ARCHITECTURE.md) names `AgentKind::Autonomous`, `capability_packs`, `delegate_to`, and `max_delegation_depth: 3`, this crate uses `Personal | Worker`, `tools` / `excluded_tools` / `denied_tools`, `delegation_targets`, and `max_delegation_depth: 2`.

### Work entry points

Every execution needs an agent id.

| Entry | `GoalSource` | What happens |
|-------|--------------|--------------|
| User chat / Magican keyboard | `User` | Chat resolves the agent via invocation policy, then answers in-thread or `delegate_to_agent` / `orchestrate_pipeline`. |
| `POST /agents/{id}/trigger` | `User` | Manual cycle. |
| `Task.schedule` fire | `Schedule` | V3 scheduler lifecycle launches the existing task's execution. |
| `AgentScheduler` + `WakeKind::Scheduled` | `Schedule` | `resume_scheduler_entry` → `SchedulerAgent` → `trigger_goal_awaitable_in_scope`. |
| Chat `delegate_to_agent` | `ChatInline` | Ephemeral task-backed cycle (`GoalTaskOptions` can keep it tracked). Soft `work_budget_secs`; no 10800s hard timer. |
| Autonomous cron on a Personal agent | `Schedule` | Focus-area goal ids `harness:{agent_id}:{slug}`. |

`AgentRoutingContext { agent_id, goal_id, cycle_id }` is the pause-store key, prefixed `agent:` so it cannot collide with `{tid}:{pid}:{sid}` execution keys.

---

## Agent definition

Parsed from `definition.agent.yaml` with `#[serde(deny_unknown_fields)]`; unknown fields fail parse. Empty `agent_id` is slugged from `name`. Full field reference: [agent-definition-reference.md](../agents/agent-definition-reference.md).

### Identity and kind

```yaml
agent_id: personal-assistant
version: 3
name: Magican
aliases: [magican]
wake_spellings: [magical, magician]   # on-device wake lexicon only
persona: |
  You are Magican…
kind: personal                         # personal | worker (default personal)
disabled: false
is_primary: true                       # Personal only
principal: alice
workspace: default
```

| Field | Role |
|-------|------|
| `disabled` | Disables this agent and reachable delegation descendants (`disabled_agent_hierarchy`). |
| `is_primary` | Primary personal agent for the workspace (`POST /agents/{id}/set-primary`). Workers rejected. |
| `onboarding_completed` | Legacy round-trip flag. |
| `app_tool` | Optional `AgentAppToolContract` so an installed app can call this agent as a sealed agent-as-tool. Absent = not a target. |
| `harness` | Personal-only. Optional `program_section` injected into focus areas that do not override `program`. |
| `social_persona` | Optional fleet-social config (`introversion`, `daily_tokens`, `opted_out`). |
| `auto_surface_policy` | Artifacts with render hints publish to a route. |
| `default_personality` | Personality-mode skill name; default `"witty"`. |
| `readable_agents` | Personal-only. Agent ids whose memory this agent may read. |
| `user_memory_isolation` | `shared` (default) or `fully_isolated` (`agents/{id}/user_memory/`). |

System agents use the `system:` id prefix and are excluded from ordinary delegation graphs.

### Field inventory

| Group | Fields |
|-------|--------|
| Identity | `agent_id`, `version` (default 1), `name`, `aliases`, `wake_spellings`, `description`, `persona` |
| Kind / lifecycle | `kind`, `disabled`, `is_primary`, `onboarding_completed`, `principal`, `workspace` |
| Tools | `tools`, `excluded_tools`, `denied_tools`, `denied_tool_params`, `browser_transports` |
| Trust / limits | `trust_level`, `constraints` |
| Memory | `memory_tiers`, `memory_consolidation`, `retention`, `user_memory_isolation`, `readable_agents` |
| Declarative | `prompt_pipeline`, `circuit_breaker`, `feedback_loops`, `notification_rules`, `llm_routing`, `strategy`, `state_machines` |
| Autonomy / harness | `autonomous_config`, `harness` |
| Surfaces | `invocation_policy`, `delegation_targets`, `chat_inline`, `auto_surface_policy`, `app_tool` |
| Persona extras | `default_personality`, `social_persona` |

Validation (`validate` / `validate_fail_fast`) is structural: identifier safety, unique memory-tier names (no `.`, no padding), non-zero retention days, Personal-only `autonomous_config` / `harness` / `readable_agents` / `is_primary`, recognised trust levels, known notification channels, valid autonomous cron, unique focus-area slugs.

### Templates and the definition store

Templates ship under `magician_data_v3/system/agent_templates/agents/{id}/definition.agent.yaml`; extra template roots overlay first-wins. `AgentDefinitionStore::for_scope` materializes missing templates into the scoped `agent_runtime` once per process per scope (in-memory `DefinitionCache`, invalidated on write). `builtin_template_definitions()` is empty: templates come from disk, and trigger admission refuses a missing definition rather than synthesizing one.

The `system/system` scope is not materialized — it is a transport fall-through (`SYSTEM_PRINCIPAL`), not a user crew.

Shipped templates: `personal-assistant` (Magican), specialist workers (`web-researcher`, `architect`, `cto`, `presentation-maker`, …), operators (`mac-operator`, `android-operator`), `system:scheduler`. Magican lists `allowed_direct_surfaces` explicitly as the defaults plus `plane`, so a terminal grant reaches it without dropping keyboard lanes.

### Invocation

`InvocationSurface` is derived from the authenticated route, never client-supplied: `chat`, `realtime_voice`, `task`, `delegation`, `handover`, `thinking_map`, `tutor`, `app_copilot`, `contextual_assist`, `public_envoy`, `meeting`, `plane`.

`public_envoy` and `meeting` are `Untrusted` audience; the rest are `Owner`. Default direct surfaces (empty allowlist) are the owner surfaces except `thinking_map` and `plane`, which need explicit opt-in. A primary agent that narrows `allowed_direct_surfaces` must still list `chat`, `task`, and `contextual_assist` or Magican keyboard lanes fail.

```yaml
invocation_policy:
  discoverability: ambient          # ambient | explicit | surface_only
  delegation: wildcard              # wildcard | explicit | none
  allowed_direct_surfaces: []       # empty = default direct surfaces
chat_inline: off                    # off | auto | confirm
delegation_targets: ["*", "web-researcher"]
```

- `chat_inline`: `off` (default) = not available to `delegate_to_agent`; `auto` runs without confirmation; `confirm` requires HITL.
- `AgentInvocationContext` binds principal, workspace, source/target agent, surface, `FeatureMode`, and `InvocationSourceKind` (`direct`, `chat_inline`, `autonomous`, `delegated`, `handover`, `product_feature`, `public`).

Effective delegation targets (`resolve_effective_delegation_target_ids_for_surface`):

1. Source must be enabled, non-system, `permits_explicit_delegation()`, with non-empty `delegation_targets`.
2. Named targets are kept only if they exist, are enabled, non-system, permit explicit delegation, and `permits_direct_surface(Delegation|Handover)`.
3. If the source allows wildcard and lists `*`, other in-scope agents that permit wildcard + that surface are appended (sorted).
4. Tool-provider schemas are projections of this set, never authorization.

`disabled_agent_hierarchy` walks `delegation_targets` from enabled Personal roots (and unreferenced workers): disabling a coordinator disables descendants no enabled root still reaches.

### Tools and browser ceiling

| Field | Semantics |
|-------|-----------|
| `tools` | Allowlist (name or category). Empty = all tools, then narrowed. |
| `excluded_tools` | Dropped from provider / deferred / structural / introspection / dispatch projections. |
| `denied_tools` | Hard deny at dispatch (always wins). |
| `denied_tool_params` | Per-tool case-insensitive parameter prefix denylist. |
| `browser_transports` | Empty = all of `cdp` / `headed` / `headless`; non-empty is an exact allowlist; unknown names fail validation. Declared on the agent so a delegated child cannot inherit the owner's Chrome via a parent carrier. |

`cdp` attaches to the owner's signed-in Chrome; `headed` / `headless` use a per-work-context `agent-browser` profile with no owner identity.

### Constraints

```rust
AgentConstraints {
    max_iterations: 4000,
    max_tokens_per_cycle: 20_000_000,
    max_consecutive_failures: 3,
    requires_approval: Vec<ApprovalRule>,
    approval_ttl_secs: 86_400,
    coordination: CoordinationConfig,
    allow_self_modification: false,
    max_duration_secs: None,           // else GOAL_PIPELINE_TIMEOUT_SECS = 10800
}
```

`CoordinationConfig`: `max_delegation_depth` (2), `delegation_timeout_secs` (300; serialized compatibility — live budgets come from the call), `allow_transitive_delegation` (false).

`ApprovalRule`: `tool`, `action` (single or list, `*` ok), optional `when` (`param_matches`, `url_contains`), optional `ttl_secs`.

```yaml
constraints:
  requires_approval:
    - tool: gmail
      action: [send, sendAs]
      when: { param_matches: { to: ["*@example.com"] } }
      ttl_secs: 3600
```

### Trust

`TrustLevel` is a string newtype: `builtin`, `local`, `reviewed`, `untrusted` (`standard` canonicalizes to `local`). An unrecognised level matches no policy and denies everything, so load validation fails loud.

`TrustPolicyEnforcer` loads `TrustPolicy { level, allow, deny }` (`ToolActionPattern`). Defaults (`builtin/trust_policies.default.yaml`):

| Level | Allow | Deny |
|-------|-------|------|
| `builtin`, `local` | `*/*` | — |
| `reviewed` | `*/*` | `report` email/slack, `treasurer` execute |
| `untrusted` | `files/read`, `search/*` | `shell/*`, `files/write`, `browser/execute` |

Seeds also live under `magician_data_v3/system/trust_policy_templates/`; per-scope files at `{agent_runtime}/system/trust_policies.yaml`.

`is_action_allowed`: a matching `deny` wins, else a matching `allow` is required. Tool match is case-insensitive; action is exact or `*`. `has_level` is case-insensitive (registration); `has_level_exact` is used for create/update/manual-trigger so a typo is a 4xx, not a silent deny-all. `consequence_class` classifies outward acts for the gate and approval matching (predicate module, not YAML).

### Declarative surfaces

Interpreted types, not a workflow YAML.

| Surface | Type | Interpreter / consumer |
|---------|------|------------------------|
| Prompt pipeline | `PromptPipelineConfig` | `PromptPipelineInterpreter` — sections with `source` xor `content`, `required`, `condition`, `format`, `filter`. `output_rules.max_context_tokens` default 4000; `truncation_priority` drops sections first |
| Circuit breaker | `CircuitBreakerPolicy` | `CircuitBreakerInterpreter` — thresholds `inject_failure_context` / `open_circuit`; recovery `user_reset` \| `time_based{cooldown_hours}`; `per_goal_override`; `idle_failure_threshold` (3), `idle_open_duration_minutes` (60) |
| Feedback loops | `FeedbackLoopDefinition` | `FeedbackLoopInterpreter`; output cached on `AgentRuntime.feedback_injection_cache` (agent_id → source_ref) |
| Evaluation | `EvaluationCriterion` | `structured_output`, `no_error`, `under_budget { max_actions }`, `memory_updated { key }`. Empty → `Inconclusive` |
| Notifications | `NotificationRule` | Declarative only (no interpreter). Channels `chat`, `webhook`, `agent_memory`. Defaults: `agent.cycle.failed` → chat (medium), `hitl.requested` → chat (high) |
| Retention | `RetentionPolicy` | Episodes / corrections / definition versions |
| LLM routing | `LlmRoutingConfig` | Lanes `planning`, `evaluation`, `correction_extraction`, `memory_consolidation`; `operations` map; optional `coding_profile` |
| Strategy | `StrategyPreference` | Default `Fixed("atomic_composition")`; also `Ordered([...])`, `AutoSelect` |
| State machines | `HashMap<String, Value>` | `StateMachineInterpreter`. Personal default: `task_lifecycle` (`idle → planning → executing → reviewing → completed`, plus `failed` and retry). Approval machine is separate |
| Artifacts | `ArtifactDeclaration` | Enrichment / render-hint copying; `enrichment_only` must not gate refinement |
| Channels | `ChannelConfig` | Optional `{agent_runtime}/system/channels.yaml`; missing → default in-app channel |

`apply_defaults()` runs only for `kind: personal`: empty tiers **and** rules → six default tiers + nine rules + 7-day episode retention (`consolidate_before_delete`); empty prompt pipeline → default sections + default feedback loops; empty circuit breaker, notifications, strategy, `state_machines` → defaults. Workers skip it.

Default prompt sections: `persona` (`definition.persona`, required), `memory_context` (`memory.episodes(limit=5)`), `task_progress` (`memory.tier[task_progress].context_summary`), `failure_context` (`feedback.failure_context`, if `has_recent_failures`), `success_patterns` (`feedback.success_patterns`, if `has_recent_successes`). Truncation order: `task_progress`, `success_patterns`, `failure_context`, `memory_context`. Other sources: `memory.user_profile`, `memory.user_knowledge`, corrections, `memory.tier[name]`, legacy `definition.goals[…]`, `feedback.*`. A required section that is empty or exceeds `max_context_tokens` is a `PromptPipelineError`.

Default circuit breaker: 1 failure → `inject_failure_context`; 3 → `open_circuit` + notify `chat`; recovery `user_reset`. `per_goal_override.max_failures` can raise the open threshold; below `effective_max`, an `open_circuit` threshold falls back to the highest matching `inject_failure_context`.

Default feedback loops (used when the definition list is empty; `run_loop_v3` emits a `FeedbackSignal` on match):

| Name | Trigger | Extract | Inject |
|------|---------|---------|--------|
| `failure_adaptation` | `episode.outcome.is_failed` | last 5 failed episodes for goal | `prompt_pipeline.failure_context` |
| `success_reinforcement` | `episode.outcome.is_succeeded` | last 10 succeeded | `prompt_pipeline.success_patterns` |
| `strategy_effectiveness` | `episode.outcome.is_completed` | `strategy_summary` | `strategy_context` |

### Autonomous config (Personal only)

```yaml
autonomous_config:
  schedule: "0 */4 * * *"
  max_tasks_per_cycle: 3          # default
  max_steps_per_plan: 10          # default
  focus_areas:
    - name: Inbox triage
      priority: medium              # low | medium | high
      schedule: "0 9 * * *"         # optional override
      program: null                 # optional program.md path
      scope: ["*"]                  # ["*"] = resolved harness scope
```

`filter_focus_areas_by_schedule` keeps areas whose effective schedule matches the firing cron; goal ids are `harness:{agent_id}:{slug}`. `build_autonomous_goal` assembles persona + matching focus areas + rendered memory tiers (`personality_profile` gets its own section). Tool catalogs travel in the function-calling tools array, not prose.

`GoalTaskOptions` (chat / handover callers) overlay the task shell without mutating the stored persona: `task_title_override`, `personality_directive` (prepended at LLM dispatch only), `lifecycle` / `sync_mode` (flip a delegate to a tracked `/tasks` row), `work_budget_secs` (soft; never cancels in-flight work), `extra_tags`, `reference_task_ids` (→ `depends_on` / linked artifacts), `browser_session_id_override` (headed Chrome stays on the parent session), `invocation_context_override` + `authorization_revision` (definition-digest seal for delegate/handover), `pre_spawn_hook` (registers fan-out before the first `agent.started`). `compose_effective_goal_desc` = optional personality prefix + stored `goal_desc`.

---

## Storage layout

`AgentStorage` roots at `…/scopes/<principal>/<workspace>/agent_runtime` or the system template root `…/system/agent_templates` — never the bare `magician_data_v3` root or a flat `magician_data_v3/agents/{id}/`. Paths are owned by `ArtifactV2Workspace::scoped_agent_runtime_root` / `system_agent_template_root`; `for_scope` stamps `principal` / `workspace` onto owned definitions.

```
agent_runtime/
  agents/
    {agent_id}/                          # memory files sit here, no nested memory/
      definition.agent.yaml
      definitions/v{n}.agent.yaml
      goal_cycles.json
      tiers/{tier}.json                  # Agent scope
      tiers/{tier}_{goal}.json           # AgentGoal scope
      episodes/ + .episode_index.json
      consolidations/
      corrections.jsonl, evidence.json, entities.json
      user_memory/                       # fully_isolated only
      state/
    scheduler_state.json + .scheduler_state.write.lock
    paused_agents.json
    approvals/
    proposals/
  users/                                 # shared user tiers (profile.json, knowledge.json, …)
  index/                                 # hybrid memory index + lancedb/
  system/                                # optional trust_policies.yaml, channels.yaml, state_machines.yaml
```

The operator feed journal is `scopes/<principal>/<workspace>/updates.jsonl`, outside `agent_runtime`. Seed `magician_data_v3/system/` holds `agent_templates`, `db_templates`, `learning`, `trust_policy_templates`, `tutor_primitives`.

- `AgentScheduler` requires scoped storage; cross-process writes take `.scheduler_state.write.lock`.
- Pause is a scope-level set in `paused_agents.json`. Trigger admission re-reads it after the lifecycle fence so a peer's pause cannot race a cached definition map. `pause` writes it and cancels in-flight tokens; `resume` removes the id.
- Private app-memory receipt paths (Unix) must be uid-owned, non-symlinked, `0700` dirs / `0600` files (`app_memory_ingress.rs`).

---

## Agent runtime

`AgentRuntime` is the in-process registry:

```text
definitions              HashMap<agent_id, AgentDefinition>
dispatch_mutexes
trigger_dispatch         per agent:goal admission (active + pending queue)
circuit_failures
evaluation / circuit_breaker / feedback interpreters
wake_up_queue
v2_orchestrator          None = admit but do not execute
artifact_v2_service      V3 task/execution shells
workspace_layout
scoped_schedulers        (principal, workspace) → AgentScheduler
cancel_tokens            keyed by scoped_automation_task_id
goal_cycles              cycle_id → AgentGoalRecord (goal_cycles.json)
feedback_injection_cache
agent_memory_resolver    V3 memory under the scoped root
```

It holds no strategy, notification, approval or memory service. Approvals resolve through `AgentApiServices` per scope; prompt assembly is `PromptPipelineInterpreter` at call sites.

### Cycle admission

`trigger_goal_awaitable_with_scope_and_overrides` is the single production entry; principal + workspace required. Missing definition, orchestrator, Artifact V3, or a paused agent abort with an empty receipt.

`admit_trigger_in_scope` → `TriggerAdmission`: `StartNow { reservation }`, `Queued { cycle_id, queue_position }` (another cycle holds the agent:goal mutex), `Duplicate { cycle_id }`, `QueueFull { cycle_id, capacity }`.

On `StartNow`:

1. Re-read the durable definition (fresh store, not boot cache) and the paused file. The in-memory definition must equal the disk record; mismatch, missing file, or paused id fails **before** task binding. Delegate/handover authorization revisions must match current definition digests.
2. Require a stateless-activated scope.
3. Create a V3 task + execution shell (`create_task_with_execution_shell`) with `agent_id`, `goal_id`, title from focus area or goal id, `approved: true`; bind the exact execution id.
4. Register a `CancellationToken` under `scoped:{hex(principal)}:{hex(workspace)}:{hex(agent_id)}:{hex(goal_id)}`.
5. Fire optional `pre_spawn_hook(execution_id, task_id)`.
6. Spawn the pipeline with outer timeout `max_duration_secs` or 10800s (not for `ChatInline`), abortable by `cancel_goal_in_scope`.
7. Persist `AgentGoalRecord` (`in_progress` → `completed` / `failed` / `cancelled` / `deferred`).

`AgentGoalRecord`: `cycle_id`, `agent_id`, `goal_id`, `principal`/`workspace`, `execution_id`, `goal_input_hash` (SHA-256), `fired_at`, `status`, `source`. `GoalTriggerReceipt`: `cycle_id`, optional `execution_id` (always set if `pre_spawn_hook` fired, so the caller can unregister fan-out), optional `task_id`.

The inner loop is `execute_agent_cycle` (`execution/agentic/executor.rs`): live-agent admission, `AgentCycleStarted` / `AgentCycleCompleted` + `publish_agent_update`, then `execute_agentically_with_refinement`. Admission happens before building the future or emitting cycle-started (rejection does not count as active). Nested `execute_agentically` inside an admitted cycle must not re-admit; continue/resume and pipeline stages must, because they drop the prior guard. Cycle start/complete wrap the loop and are not journalled through the phase outbox.

`POST /agents/{id}/trigger` body is optional JSON `{ "goal_id", "trigger" }`; empty is valid; non-JSON Content-Type with a body is 415. The handler hydrates definition, trust enforcer, and approval rules, then calls the same path with `GoalSource::User`.

---

## Scheduler, wakes, and `Task.schedule`

Two scheduling planes share `WakeUpQueue`.

### `Task.schedule`

```text
kind: Cron { expression, timezone } | Interval { seconds, jitter_seconds }
    | Once { at } | OnEvent { event_pattern }
timezone: Option<String>
missed_fire_policy: skip | run_once | queue     (default skip)
concurrent_execution_policy: skip | queue | cancel_previous
execution_history_retention: max_records / max_age_days
max_runs: Option<u32>                           # counter: TaskState.schedule_fire_count
paused: Option<bool>                            # schedule pause ≠ execution pause
```

Enforced at boot hydration (`list_scheduled_tasks_across_scopes`) and in the dispatch loop. Fires are launched by the V3 scheduler lifecycle, never by inventing a replacement execution.

`Task` fields the agent runtime consumes: `agent_id`, `schedule`, `created_by` (`User` / `Agent { agent_id }` / `Delegation { parent_task_id, delegating_agent_id }`), `approved` (default true), `depends_on` / `linked_task_ids`, `auto_surface_policy`, `retry_at` when `Deferred`. Task status is Artifact's lifecycle, not an agent state machine.

### `AgentScheduler`

Durable per-scope `scheduler_state.json`:

```text
SchedulerEntryState {
  agent_id, goal_id, task_id: Option<String>, trigger_seq, last_triggered,
  registration: Cron { schedule, timezone, next_run_at } | Event { pattern, filter } | Idle
}
```

- `resume_scheduler_agent_in_scope` builds `ScheduleContext` from the registration: Cron uses expression + timezone, Event its pattern, `Idle` becomes `Interval { 300s, jitter 30s }`. `last_fire` from `goal_last_fire_time_in_scope`; missed fires from `schedule_utils::count_missed_fires`. `SchedulerAgent` (`pipeline/system_agents`) returns `Completed` (fire the goal) or `Sleeping { wake_at }` (requeue).
- `compute_next_run_at` evaluates 5-field cron in UTC, fixed offset, or IANA zone, returning UTC.
- On `Failed` / `WaitingForUser` / error the runtime reschedules the next fire rather than orphaning the entry. Disabled hierarchy members are unregistered and their wakes cancelled.
- `GET /api/magician/v2/triggers` lists these entries (optional `agent_id` filter).

### `WakeUpQueue`

Dumb timer: sorted `WakeEntry`, `drain_due()`, 30s durable claim lease; limits 4096 entries / 4 MiB file / 64 concurrent dispatches. No cron logic.

| `WakeKind` | Dispatch |
|------|----------|
| `Scheduled` | `resume_scheduler_entry` → `SchedulerAgent` → trigger (`GoalSource::Schedule`) or re-schedule on `Sleeping` |
| `TaskSchedule` | Recovery hint for an already-accepted active execution; not authority to create a replacement |
| `TaskRetry` | Deferred task retry through ordinary Artifact admission |
| `ExecutionRetry` | Re-adopt **this** execution after a placement-pin sleep; requires principal, workspace, task_id, execution_id (each ≤ 4 KiB) |
| `ChildCompleted` | Reconcile parent `active_delegation_group` |

`WakeConsumer::ProductionWeb` owns every kind; `RuntimeLegacy` must not claim `TaskSchedule` / `TaskRetry`. `resume_scheduler_entry` parses the `scoped:` hex id first, else `entry_for_task_id`. Claimed timestamps are the lease deadline; `execution_retry_due_at` keeps the original due instant. A claim I/O failure backs off 1s in memory without dropping the persisted row.

---

## Memory

Types: `magician-vector-index/src/memory_tiers.rs` (re-exported as `agents::memory_tiers`). I/O: `AgentMemoryService` (file-backed JSON for tiers, episodes, corrections, user profile/knowledge) and `AgentMemoryResolver` (scoped V3 root, no process-global dir). Consolidation: `MemoryTierInterpreter` + `MemoryConsolidator`; `consolidate_cycle_completed_v3` applies matching rules after a cycle, skipping paused episodes. Prompt injection: `memory_prompt_blocks.rs` (500 ms first-decision budget; see the execution README).

`MemoryTierDefinition`: `name`, `scope` (`agent` | `agent_goal` | `user`), `description`, `schema`, `render` (`format` + `template`), `retention` (`forever` | `days(n)` | `goal_lifetime`).

Default tiers (Personal, when YAML omits tiers and rules):

| Name | Scope | Retention |
|------|-------|-----------|
| `entities` | agent | forever |
| `insights` | agent | forever |
| `recent_activity` | agent | 30 days |
| `archive` | agent | forever |
| `task_progress` | agent_goal | goal lifetime |
| `environment_knowledge` | agent | forever |

No `knowledge` or `code_knowledge` default; coding workers declare `codebase_knowledge` / `architectural_knowledge`. See [ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md](./ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md).

Default consolidation rules:

| Name | Trigger | Target |
|------|---------|--------|
| `extract_entities` | `StepCompleted` | `entities` |
| `extract_insights` | `StepCompleted` | `insights` |
| `summarize_recent_activity` | batch (min 10 episodes / 24h) | `recent_activity` |
| `update_task_progress` | `StepCompleted` | `task_progress` (`MapEpisodeToTask`) |
| `distill_insights` | batch 24h | `insights` |
| `archive_old_episodes` | batch 48h | `archive` |
| `expire_old_episodes` | `RetentionExpiry` | `archive` |
| `extract_environment_knowledge` | batch | `environment_knowledge` |
| `promote_to_user` | batch 72h | `user.preferences` |

Transforms are `Llm` (`MemoryConsolidationOperation` + `$ref:` prompt) or `Structured` (`BuiltinTransform`). Merge: `UpsertByName`, `UpsertBySimilarity`, `AppendPeriod`. Episode retention: 7 days with defaults injection, else `EpisodeRetention` default 90 days.

Hybrid index: `{agent_runtime}/index/{manifest.json,documents.jsonl,lancedb/}`; definition writes mark it dirty. `memory_utility_reviewer` labels injected memories after a run (max 12 candidates; queue `utility_review_queue.json`) for the temperature overlay. `memory_hot_projections` persist T0 lanes beside the index.

User-scoped files: `shared` → `{agent_runtime}/users/{tier}.json`, `profile.json`, `knowledge.json`, `work_evidence.json`, `work_entities.json`; `fully_isolated` → `agents/{id}/user_memory/`. `GET /agents/{id}/memory/{tier}` requires `goal` for an `agent_goal` tier when more than one goal could apply.

---

## Approvals

1. **`ApprovalGate`** — pure match of `ExecutionPlan` steps against `constraints.requires_approval` → `Approved` | `NeedsApproval { pending_actions }`.
2. **`ApprovalService`** — durable requests in `{agent_runtime}/agents/approvals/`, driven by `StateMachineInterpreter` using the built-in `approval` machine (or a compatible `{agent_runtime}/system/state_machines.yaml`):

```
pending  --decision_approve--> validating --(plan hash valid)--> approved
                             \--(plan hash invalid)--> rejected
pending  --decision_reject--> rejected
pending|validating --ttl_elapsed--> expired
pending --delivery_failed--> rejected
```

TTL default 86400s; max 4 resolve attempts; plan-hash drift is `PlanDrift`. Fan-out uses enabled `ChannelConfig`s or a default in-app channel. On the wire HITL is `hitl.requested` / `hitl.resolved`.

`ApprovalRequest`: `approval_id`, `agent_id`, `goal_id`, `trigger_seq`, `cycle_id`, `execution_id`, `pending_actions`, `plan_hash`, `status`, `created_at`, `expires_at`, `resolved_by`/`resolved_at`, `principal`/`workspace` (stamped so a resolve from any surface is announced to the requesting scope). Delivery states: `sent` / `resolved` / `dismissed` / `failed`.

HTTP under `/api/magician/v2/approvals`; resolve maps to `ApprovalDecision::{Approve,Reject}`. Preview checks never waive and never debit envelopes. The separate envelope system (`approval_envelopes/`) waives covered acts and is not this gate.

---

## Proposals

`ProposalStore` persists `DefinitionProposal` (UUID ids, `yaml_before` / `yaml_after`) under `{agent_runtime}/agents/proposals/`. Status `pending` | `approved` | `rejected` | `deferred`; decisions `approve` | `reject` | `defer`. Apply re-validates YAML and trust policy before writing (`ProposalApplicationError`); stale transitions fail closed; a crash between `update_definition` and `mark_applied` is recovered. Create requires non-empty `source` and a valid agent id.

---

## Agent updates and observability

### Operator feed

`AgentUpdate` is flat JSON: envelope (`id` ULID, `ts` millis, `workspace_id`, `agent_id`, optional `thread_id` / `cycle_id`) + internally tagged `kind`. Emit only via `publish_agent_update` / `agent_update_envelope` (`event_type = "agent.update"`). One writer per process appends `scopes/<principal>/<workspace>/updates.jsonl` (rotate at 50 MiB; rotated files are archival). `GET /api/magician/v2/updates` tails the current file (`since`, `limit` default 200 max 1000, `agent`, `kind`, `thread`); bearer scope is authoritative (client `workspace` ignored) and path-escaping ids are rejected.

`AgentUpdateKind`: `agent_created|updated|deleted|paused|resumed`, `cycle_started|completed|failed|paused`, `goal_completed|failed|recovered`, `approval_requested|resolved|expired`, `artifact_created`, `artifact_create_failed`, `circuit_opened|recovered`, `task_created|updated|completed|failed`, `memory_report`, `feedback_generated`, `tier_consolidated`, `delegation_issued|resolved`, `feed_stalled`, `published_surface_changed`.

Cycle start/complete and HITL are emitted inline beside canonical `agent.cycle.*` / `hitl.*`; `agent_update_from_legacy_event` does not double-fire them. `FeedStallWatchdog` (one bus subscriber; tick 60s, stall after 600s without a terminal cycle update, GC 24h) emits `FeedStalled { silent_for_ms }` once per cycle.

### Agentic loop events

`RuntimeTransportEvent` (`realtime_events.rs`): `AgenticExecutionStarted`, `AgenticIterationStarted`, `AgenticIterationCompleted`, `AgenticStepStuckWarning`, `AgenticPageUnderstanding`, `AgenticDecisionMade`, `AgenticActionExecuted`, `AgenticExecutionCompleted`, `AgenticWaitingForConfirmation`, `AgenticWaitingForUser`, `AgenticResumed`. Loop behaviour is specified in [AGENTIC_EXECUTION_DESIGN.md](./AGENTIC_EXECUTION_DESIGN.md) and [FLAT_LOOP.md](./FLAT_LOOP.md).

`AgenticWaitingFor*` are lifecycle markers; the human-response payload is `HitlRequested { source: "agentic" }`. `AgenticResumed` records `resumed_from_iteration`, `input_type`, `user_responded`.

---

## Outward gate

`outward_gate.rs` is the fail-closed predicate layer before any act that leaves the machine (mail, chat, calendar, forms), independent of `ApprovalGate`:

1. May this actor use this capability in this work? (`work_context_refusal`)
2. May we contact this person? (`contact_refusal` / suppression register)
3. Was the dispatched act acknowledged? (`DispatchLog` + `DeliveryLedger::unreconciled`)

Absence is refusal. Empty recipients refuse only when empty means "failed to parse who this reaches"; class-derived addressing (own calendar, site form) may send without the register. Receipts: `outward_receipts.rs`.

---

## API surface

Handlers are in `magician-api/src/web_api.rs`; agent routes require an authenticated principal/workspace scope. Prefix `/api/magician/v2`.

| Method | Path | Role |
|--------|------|------|
| POST / GET | `/agents` | Create (JSON or YAML) / list (`{ agents, system_agents, total_count }`, hydrates templates first) |
| POST | `/agents/refresh-definitions` | Reload scoped YAML |
| GET | `/agents/health`, `/agents/{id}/health` | Crew / agent health |
| GET / PUT / PATCH / DELETE | `/agents/{id}` | PATCH is RFC 7386 merge patch |
| GET | `/agents/{id}/harness-overview` | Harness / focus-area view |
| GET | `/agents/{id}/effective-tools` | Resolved tool snapshot (after tools/excluded/denied/trust/surface) |
| GET / POST | `/agents/{id}/runtime-context-cache[/refresh]` | Hydrated prompt/runtime bundle / rebuild |
| POST | `/agents/{id}/set-primary` | Flip `is_primary` from current primary to target |
| POST | `/agents/{id}/consolidate-user-memory` | User-promotion / shared-tier consolidation |
| POST | `/agents/{id}/trigger`, `/pause`, `/resume` | Manual cycle; durable pause / clear |
| GET | `/agents/{id}/episodes`, `/artifacts`, `/memory`, `/memory/consolidation-health`, `/memory/{tier}` | Read views |
| GET | `/triggers` | Scheduler entries |
| GET / POST | `/approvals`, `/approvals/{id}`, `/approvals/{id}/resolve` | Approvals |
| GET / POST | `/proposals`, `/proposals/{id}`, `/proposals/{id}/resolve` | Proposals |
| GET | `/updates` | Operator feed (`agent_updates_api.rs`) |
| GET / PUT | `/agents/{id}/layout` | GAUI layout (`gaui_api.rs`) |

Create emits `agent.created`, reconciles progress subscriptions, and signals definition change. Not implemented: `/agents/{id}/correct`, `/export`, `/agents/import`, `/gallery/*`, `/delegations/*`, `/agents/{id}/delegations`, `/workflows/*`. Delegation is `delegation_targets` + invocation policy + the executor's `DelegationDispatcher`.

### Capability catalog cache

The scope capability catalog behind `effective-tools` and `runtime-context-cache` is a snapshot keyed by a digest of the scope's `SKILL.md` files, not reloaded on a timer. On a cache hit, at most once per 30 s per scope, a background task recomputes the digest and, if changed, **builds and validates a candidate before swapping**. The candidate replaces the served snapshot only if it does not newly fail a skill the served snapshot loaded — so a half-saved `SKILL.md`, mid-checkout tree, or unknown manifest field cannot silently drop a skill. Holds are reported on cache status (`held_back_scopes` with served/observed revisions and failing skills, `held_back_revisions`, `swaps`). Removed skills and broken new skills do not hold the catalog. Swaps are in place, so a scope always has a snapshot.

Parsed pack definitions are cached one level down in `CapabilityWorkspaceManager` (`load_pack_defs_for_scope`) because `LocalToolServices::all_tools` is asked once per delegation target during bootstrap, and re-parsing every `SKILL.md` each time dominated spawn latency for `delegation_targets: ["*"]`. They revalidate on the same 30 s digest cadence (scope skills root, extra skills roots, Task Recipe catalog) or on `invalidate_scope` / `clear_cache`. Candidate builds read disk via `load_pack_defs_for_scope_fresh` so validation never checks the served copy. `scope_pack_cache_status()` reports `hits`, `revalidations`, `builds`, `invalidations`.

---

## Capability dispatch (agents as consumers)

Agents do not own a second execution engine. Tools are YAML packs executed by `PackCapabilityProvider` (`execution/pack_provider.rs`); `lower_step_with_registry` is the registry path. `ImplementationType`: `Composite` (sequence of capabilities), `Compiled` (Rust provider by `provider_name`), `Primitive` (nested inner loop), `Command` (subprocess: `program`, `fixed_args`, `arg_mappings`, …). There is no `JavaScript` variant. Native vs browser is `requires_browser_session()` on the provider.

Packs materialize per scope under `capabilities/packs/` (`magician_data_v3/system/capability_templates/packs/` is not a live seed). Skills load from `<system>/skills/` plus per-scope `capabilities/skills/`; there is no filesystem watcher or `POST /capabilities/reload`. A new scope picks up disk skills on first turn via `ScopedCapabilityResolver::registry_for_scope`; already-loaded scopes need a restart.

---

## Related

- [UNIFIED_AGENTIC_ARCHITECTURE.md](./UNIFIED_AGENTIC_ARCHITECTURE.md) — concept model (some kind/pack names are design-ahead of this crate)
- [AGENTIC_EXECUTION_DESIGN.md](./AGENTIC_EXECUTION_DESIGN.md) — inner loop
- [FLAT_LOOP.md](./FLAT_LOOP.md) — catalog and per-action loop
- [IN_CONTEXT_DELEGATION.md](./IN_CONTEXT_DELEGATION.md) — in-execution owner hops
- [ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md](./ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md) — environment tier
- [TASK_STATE_DESIGN.md](./TASK_STATE_DESIGN.md) — durable task state
- [USER_ESCALATION_ARCHITECTURE.md](./USER_ESCALATION_ARCHITECTURE.md) — pause/resume on exhausted failure
- [storage-v2-format.md](../storage-v2-format.md) — task/execution on disk
- Historical: AGENTIC_EXECUTION_MVP.md, TRUE_AGENTS_EXECUTION_BOARD.md, TRUE_AGENTS_PHASE3.md, TRUE_AGENTS_PHASE4.md, TRUE_AGENTS_PHASE5.md
