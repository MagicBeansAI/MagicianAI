# True Agents: Runtime Contract

> **Code**: `magician/src/magician_v2/agents/`
> **HTTP**: `magician-api/src/web_api.rs`, `magician-api/src/agent_updates_api.rs`

As-built contract for persistent agents: `AgentDefinition`, scoped `agent_runtime` storage, `AgentScheduler` + `Task.schedule`, `WakeUpQueue`, cycle admission, memory, approvals, proposals, trust, `agent.update` events.

Not in the runtime: agent-YAML goals or triggers, `run_cycle`, `AgentIdleService`, `IdleBehavior`, `WorkflowInterpreter`, `WorkflowDefinition`, `/api/magician/v2/workflows/*`, `AgentKind::Autonomous`, meta-agent templates. Agent packs, gated creativity and browser overlays are archive/future material.

---

## Scope

An **agent** is the durable executor (identity, persona, tool ceiling, trust, memory, invocation policy); a **task** is work assigned to it, and conversation context lives on the task and its executions (`ExecutionRun`). The chat grouping is `ui_thread_id` (default `"general"`); `V2Thread` is gone.

| Concern | Primary files (`agents/`) |
|---------|----------------|
| Definition types + validation | `types.rs` |
| Scoped YAML store | `definition_store.rs`, `storage.rs` |
| In-process registry + cycle admission | `runtime.rs` |
| Cron / event / idle registrations | `scheduler.rs` |
| Durable timers | `wake_up_queue.rs` |
| Autonomous goal text | `autonomous_goal.rs` |
| Memory I/O, tiers, consolidation | `memory.rs`, `memory_defaults.rs`, `memory_tier_interpreter.rs`, `memory_consolidator.rs`; types from `magician-vector-index` (`agents::memory_tiers`) |
| Approvals | `approval.rs`, `approval_service.rs`, `approval_store.rs`, `state_machine.rs` |
| Definition proposals | `proposals.rs` |
| Trust policies | `trust.rs`, `builtin/trust_policies.default.yaml` |
| Operator feed | `agent_update.rs`, `agent_update_emitter.rs`, `agent_update_journal.rs` |
| Outward send gate | `outward_gate.rs`, `outward_actions.rs`, `outward_receipts.rs` |

Agents admit a cycle and hand it a V3 task + execution shell; the inner loop is `execution/agentic/executor.rs`, not reimplemented here. Also: `condition.rs` (prompt-section conditions), `task_ownership.rs`, `personal_agent_retrieval.rs` (app personal-agent retrieval authority), `project_knowledge.rs`, `app_memory_ingress.rs` (sealed app-memory receipts), `events.rs` (`emit_agent_cycle_started` / `completed` / `triggered`). `memory_candidates`, `memory_index`, `memory_temperature`, `memory_tier_health`, `retrieval_scope`, `memory_tiers` are facades over `magician-vector-index`. There is no `delegation.rs`, `notifications.rs`, or `workflow_interpreter.rs`.

---

## Concept model (as-built)

An agent (`Personal | Worker`) has no `goals[]` or `triggers[]`. A task has a compulsory `agent_id`, `schedule: Option<TaskSchedule>` (`storage/task_models.rs`), `created_by: User | Agent | Delegation` and Artifact V3 task + execution rows. Cycles carry a `goal_id` string (focus-area slug, scheduler key or caller id), not a YAML goal. Where [UNIFIED_AGENTIC_ARCHITECTURE.md](./UNIFIED_AGENTIC_ARCHITECTURE.md) says `AgentKind::Autonomous`, `capability_packs`, `delegate_to`, `max_delegation_depth: 3`, this crate uses `Personal | Worker`, `tools` / `excluded_tools` / `denied_tools`, `delegation_targets`, `max_delegation_depth: 2`.

### Work entry points

Every execution needs an agent id.

| Entry | `GoalSource` | What happens |
|-------|--------------|--------------|
| User chat / Magican keyboard | `User` | Invocation policy picks the agent; in-thread or `delegate_to_agent` / `orchestrate_pipeline`. |
| `POST /agents/{id}/trigger` | `User` | Manual cycle. |
| `Task.schedule` fire | `Schedule` | V3 scheduler launches the existing task's execution. |
| `AgentScheduler` + `WakeKind::Scheduled` | `Schedule` | `resume_scheduler_entry` → `SchedulerAgent` → `trigger_goal_awaitable_in_scope`. |
| Chat `delegate_to_agent` | `ChatInline` | Ephemeral task-backed cycle (`GoalTaskOptions` can keep it tracked); soft `work_budget_secs`, no 10800s hard timer. |
| Autonomous cron on a Personal agent | `Schedule` | Goal ids `harness:{agent_id}:{slug}`. |

The pause-store key `AgentRoutingContext { agent_id, goal_id, cycle_id }` is prefixed `agent:` so it never collides with `{tid}:{pid}:{sid}` execution keys.

---

## Agent definition

Parsed from `definition.agent.yaml` with `#[serde(deny_unknown_fields)]`; empty `agent_id` is slugged from `name`. Every field, default and example (identity, `kind`, tools, `browser_transports`, `AgentConstraints`, `ApprovalRule`, `CoordinationConfig`, memory, `prompt_pipeline`, `llm_routing`, `autonomous_config`, `harness`, `app_tool`, `social_persona`) is in [agent-definition-reference.md](../agents/agent-definition-reference.md).

Runtime rules: `disabled_agent_hierarchy` walks `delegation_targets` from enabled Personal roots (and unreferenced workers), disabling descendants no enabled root still reaches. `browser_transports` sits on the agent so a delegated child cannot inherit the owner's Chrome (`cdp`) through a parent carrier. `max_duration_secs: None` means `GOAL_PIPELINE_TIMEOUT_SECS = 10800`. `system:`-prefixed agents stay out of ordinary delegation graphs. `denied_tools` always win at dispatch. `validate` / `validate_fail_fast` are structural (identifiers, tier names, non-zero retention, Personal-only fields, trust levels, channels, cron, focus-area slugs).

### Templates and the definition store

Templates ship under `magician_data_v3/system/agent_templates/agents/{id}/definition.agent.yaml` (extra roots overlay first-wins); `AgentDefinitionStore::for_scope` materializes missing ones into the scoped `agent_runtime` once per process per scope (`DefinitionCache`, invalidated on write). `builtin_template_definitions()` is empty, so admission refuses a missing definition. `system/system` (`SYSTEM_PRINCIPAL`) is never materialized. Shipped: `personal-assistant` (Magican, defaults plus `plane` in `allowed_direct_surfaces`), workers (`web-researcher`, `architect`, `cto`, `presentation-maker`, …), operators (`mac-operator`, `android-operator`), `system:scheduler`.

### Invocation

`invocation_policy` gates who may invoke an agent. `InvocationSurface` comes from the route, never the client: `chat`, `realtime_voice`, `task`, `delegation`, `handover`, `thinking_map`, `tutor`, `app_copilot`, `contextual_assist`, `public_envoy`, `meeting`, `plane`. `public_envoy` and `meeting` are `Untrusted`, the rest `Owner`; an empty allowlist = owner surfaces minus `thinking_map` and `plane`. A narrowed primary agent must keep `chat`, `task`, `contextual_assist` for Magican keyboard lanes. `chat_inline` is `off` (default), `auto` or `confirm` (HITL). `AgentInvocationContext` binds scope, source/target agent, surface, `FeatureMode` and `InvocationSourceKind` (`direct`, `chat_inline`, `autonomous`, `delegated`, `handover`, `product_feature`, `public`).

`resolve_effective_delegation_target_ids_for_surface` keeps only enabled, non-system targets that exist and pass `permits_explicit_delegation()` and `permits_direct_surface(Delegation|Handover)` (the source must be enabled, non-system, explicit-delegating and list targets); `*` on a wildcard-allowing source adds in-scope agents that permit wildcard + that surface. Tool-provider schemas project this set and never authorize.

### Trust

`TrustLevel`: `builtin`, `local`, `reviewed`, `untrusted` (`standard` → `local`); an unrecognised level matches no policy and denies everything, so validation fails loud. `TrustPolicyEnforcer` loads `TrustPolicy { level, allow, deny }` (`ToolActionPattern`) from `builtin/trust_policies.default.yaml` (seeds in `magician_data_v3/system/trust_policy_templates/`, per scope `{agent_runtime}/system/trust_policies.yaml`):

| Level | Allow | Deny |
|-------|-------|------|
| `builtin`, `local` | `*/*` | — |
| `reviewed` | `*/*` | `report` email/slack, `treasurer` execute |
| `untrusted` | `files/read`, `search/*` | `shell/*`, `files/write`, `browser/execute`, `report/*`, `treasurer` execute |

`is_action_allowed`: a matching `deny` wins, else an `allow` is required (tool case-insensitive, action exact or `*`). Registration uses case-insensitive `has_level`; create/update/manual-trigger use `has_level_exact`, so a typo is a 4xx, not a silent deny-all. `consequence_class` classifies outward acts for the gate and approval matching.

### Declarative surfaces

Interpreted types, not workflow YAML:

| Surface | Type | Interpreter / runtime rule |
|---------|------|------------------------|
| Prompt pipeline | `PromptPipelineConfig` | `PromptPipelineInterpreter`; `max_context_tokens` 4000 |
| Circuit breaker | `CircuitBreakerPolicy` | `CircuitBreakerInterpreter`; `idle_failure_threshold` (3), `idle_open_duration_minutes` (60) |
| Feedback loops | `FeedbackLoopDefinition` | `FeedbackLoopInterpreter`; cached on `AgentRuntime.feedback_injection_cache` |
| Evaluation | `EvaluationCriterion` | `structured_output`, `no_error`, `under_budget { max_actions }`, `memory_updated { key }`; empty → `Inconclusive` |
| Notifications | `NotificationRule` | No interpreter; defaults `agent.cycle.failed` → chat (medium), `hitl.requested` → chat (high) |
| Retention | `RetentionPolicy` | Episodes, corrections, versions |
| LLM routing | `LlmRoutingConfig` | Lanes, `operations`, `coding_profile` |
| Strategy | `StrategyPreference` | `Fixed("atomic_composition")` default; `Ordered([...])`, `AutoSelect` |
| State machines | `HashMap<String, Value>` | `StateMachineInterpreter`; Personal default `task_lifecycle` (`idle → planning → executing → reviewing → completed`, `failed`, retry); the approval machine is separate |
| Artifacts | `ArtifactDeclaration` | Render-hint copying; `enrichment_only` must not gate refinement |
| Channels | `ChannelConfig` | Optional `{agent_runtime}/system/channels.yaml`; else default in-app channel |

`apply_defaults()` runs only for `kind: personal`: empty tiers **and** rules → six tiers, nine rules, 7-day episode retention (`consolidate_before_delete`); each empty surface gets its default.

- Prompt sections: `persona` (`definition.persona`, required), `memory_context` (`memory.episodes(limit=5)`), `task_progress` (`memory.tier[task_progress].context_summary`), `failure_context` (`feedback.failure_context`, if `has_recent_failures`), `success_patterns` (`feedback.success_patterns`, if `has_recent_successes`); truncated `task_progress` first, then `success_patterns`, `failure_context`, `memory_context`. `definition.goals[…]` sources fail (goals live on Task). A required section that is empty or exceeds `max_context_tokens` is a `PromptPipelineError`.
- Circuit breaker: 1 failure → `inject_failure_context`, 3 → `open_circuit` + notify `chat`, recovery `user_reset`; below `effective_max` (raised by `per_goal_override.max_failures`) `open_circuit` falls back to the highest matching `inject_failure_context`.
- Feedback loops (when the list is empty; `run_loop_v3` emits a `FeedbackSignal` on match): `failure_adaptation` (`episode.outcome.is_failed`, last 5 failed for the goal → `prompt_pipeline.failure_context`), `success_reinforcement` (`episode.outcome.is_succeeded`, last 10 → `prompt_pipeline.success_patterns`), `strategy_effectiveness` (`episode.outcome.is_completed`, `strategy_summary` → `strategy_context`).

### Autonomous config (Personal only)

`filter_focus_areas_by_schedule` keeps focus areas whose effective schedule matches the firing cron (goal ids `harness:{agent_id}:{slug}`). `build_autonomous_goal` assembles persona, matching focus areas and rendered tiers (`personality_profile` in its own section); tool catalogs go in the tools array, not prose.

`GoalTaskOptions` let chat / handover callers overlay the task shell without touching the stored persona: `task_title_override`, `personality_directive` (prepended at LLM dispatch only), `lifecycle` / `sync_mode` (make a delegate a tracked `/tasks` row), `work_budget_secs` (soft; never cancels in-flight work), `extra_tags`, `reference_task_ids` (→ `depends_on` / linked artifacts), `browser_session_id_override` (headed Chrome stays on the parent session), `browser_tab_label` (the run's tab-group name in the owner's Chrome, sent to Magicutor as `?label=`; chat launches pass the conversation's title, or on its first turn the message that started the run since a conversation is titled after that turn; other runs fall back to the task title), `invocation_context_override` + `authorization_revision` (definition-digest seal), `pre_spawn_hook` (fan-out registered before the first `agent.started`). `compose_effective_goal_desc` = personality prefix + stored `goal_desc`.

---

## Storage layout

`AgentStorage` roots at `…/scopes/<principal>/<workspace>/agent_runtime` (`ArtifactV2Workspace::scoped_agent_runtime_root`) or `…/system/agent_templates` (`system_agent_template_root`), never the bare `magician_data_v3` root or a flat `magician_data_v3/agents/{id}/`; `for_scope` stamps `principal` / `workspace` onto owned definitions.

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

The feed journal `scopes/<principal>/<workspace>/updates.jsonl` sits outside `agent_runtime`. Seed `magician_data_v3/system/` holds `agent_templates`, `db_templates`, `learning`, `trust_policy_templates`, `tutor_primitives`.

`AgentScheduler` requires scoped storage; cross-process writes take `.scheduler_state.write.lock`. Pause is a scope-level set in `paused_agents.json` (`pause` also cancels in-flight tokens, `resume` removes the id), re-read after the lifecycle fence so a peer's pause cannot race admission. App-memory receipt paths (Unix) must be uid-owned, non-symlinked `0700` dirs / `0600` files (`app_memory_ingress.rs`).

---

## Agent runtime

`AgentRuntime` is the in-process registry: definitions, `trigger_dispatch` (per agent:goal active + pending queue), `wake_up_queue`, `v2_orchestrator` (`None` = admit but do not execute), `scoped_schedulers`, `cancel_tokens` (by `scoped_automation_task_id`), `goal_cycles` (`goal_cycles.json`), interpreters and `agent_memory_resolver`. It holds no strategy, notification, approval or memory service: approvals resolve through `AgentApiServices` per scope, prompt assembly is `PromptPipelineInterpreter` at call sites.

### Cycle admission

`trigger_goal_awaitable_with_scope_and_overrides` is the single production entry (principal + workspace required); a missing definition, orchestrator or Artifact V3, or a paused agent, yields an empty receipt. `admit_trigger_in_scope` → `TriggerAdmission`: `StartNow { reservation }`, `Queued { cycle_id, queue_position }` (another cycle holds the agent:goal mutex), `Duplicate { cycle_id }`, `QueueFull { cycle_id, capacity }`. On `StartNow`:

1. Re-read the durable definition (fresh store, not boot cache) and paused file in a stateless-activated scope; a definition differing from disk, a missing file, a paused id or a stale delegate/handover authorization revision fails **before** task binding.
2. `create_task_with_execution_shell` (`agent_id`, `goal_id`, title from focus area or goal id, `approved: true`), bind that execution id, register a `CancellationToken` under `scoped:{hex(principal)}:{hex(workspace)}:{hex(agent_id)}:{hex(goal_id)}`, fire optional `pre_spawn_hook(execution_id, task_id)`.
3. Spawn with outer timeout `max_duration_secs` or 10800s (not for `ChatInline`), abortable by `cancel_goal_in_scope`.
4. Persist `AgentGoalRecord` (`cycle_id`, `agent_id`, `goal_id`, `principal`/`workspace`, `execution_id`, `goal_input_hash` SHA-256, `fired_at`, `source`; status `in_progress` → `completed` / `failed` / `cancelled` / `deferred`).

`GoalTriggerReceipt` (`cycle_id`, `execution_id`, `task_id`) always carries `execution_id` once `pre_spawn_hook` fired, so the caller can unregister fan-out.

`execute_agent_cycle` admits before `AgentCycleStarted` (rejections never count as active), publishes `AgentCycleStarted` / `AgentCycleCompleted` via `publish_agent_update` (not the phase outbox), then runs `execute_agentically_with_refinement`. Nested `execute_agentically` must not re-admit; continue/resume and pipeline stages must, since they drop the prior guard. `POST /agents/{id}/trigger` takes optional JSON `{ "goal_id", "trigger" }` (empty is valid; non-JSON Content-Type with a body is 415), hydrates definition, trust enforcer and approval rules, and uses `GoalSource::User`.

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

Enforced at boot hydration (`list_scheduled_tasks_across_scopes`) and in dispatch; fires never create a replacement execution. The runtime also reads `created_by` (`Agent { agent_id }` / `Delegation { parent_task_id, delegating_agent_id }`), `approved` (default true), `depends_on` / `linked_task_ids`, `auto_surface_policy`, `retry_at` when `Deferred`. Task status is Artifact's lifecycle.

### `AgentScheduler`

Per-scope `scheduler_state.json`:

```text
SchedulerEntryState {
  agent_id, goal_id, task_id: Option<String>, trigger_seq, last_triggered,
  registration: Cron { schedule, timezone, next_run_at } | Event { pattern, filter } | Idle
}
```

`resume_scheduler_agent_in_scope` builds `ScheduleContext` (Cron: expression + timezone; Event: its pattern; `Idle` = `Interval { 300s, jitter 30s }`; `last_fire` from `goal_last_fire_time_in_scope`; missed fires via `schedule_utils::count_missed_fires`); `SchedulerAgent` (`pipeline/system_agents`) returns `Completed` (fire) or `Sleeping { wake_at }`. `compute_next_run_at` evaluates 5-field cron in UTC, a fixed offset or an IANA zone, returning UTC. `Failed` / `WaitingForUser` / errors reschedule the next fire rather than orphan the entry; disabled hierarchy members are unregistered and their wakes cancelled. `GET /api/magician/v2/triggers` lists entries (optional `agent_id`).

### `WakeUpQueue`

No cron logic: sorted `WakeEntry`, `drain_due()`, 30s durable claim lease, max 4096 entries / 4 MiB file / 64 concurrent dispatches.

| `WakeKind` | Dispatch |
|------|----------|
| `Scheduled` | `resume_scheduler_entry` → `SchedulerAgent` → trigger (`GoalSource::Schedule`) or re-schedule on `Sleeping` |
| `TaskSchedule` | Recovery hint for an accepted execution, not authority to replace it |
| `TaskRetry` | Deferred task retry via ordinary Artifact admission |
| `ExecutionRetry` | Re-adopt **this** execution after a placement-pin sleep; requires principal, workspace, `task_id`, `execution_id` (each ≤ 4 KiB) |
| `ChildCompleted` | Reconcile parent `active_delegation_group` |

`WakeConsumer::ProductionWeb` owns every kind; `RuntimeLegacy` must not claim `TaskSchedule` / `TaskRetry`. `resume_scheduler_entry` tries the `scoped:` hex id, then `entry_for_task_id`. Claimed timestamps are the lease deadline (`execution_retry_due_at` keeps the original); a claim I/O failure backs off 1s in memory and keeps the persisted row.

---

## Memory

Types: `magician-vector-index/src/memory_tiers.rs`. `AgentMemoryService` is file-backed JSON (tiers, episodes, corrections, user profile/knowledge) under the scoped root from `AgentMemoryResolver` (no process-global dir); `MemoryTierInterpreter` + `MemoryConsolidator` consolidate (`consolidate_cycle_completed_v3` after a cycle, skipping paused episodes). `memory_prompt_blocks.rs` injects memory within the first-decision context budget (`agent_surface_runtime.context_retrieval.autonomous_task.deadline_ms`, 500 ms). `MemoryTierDefinition`: `name`, `scope` (`agent` | `agent_goal` | `user`), `description`, `schema`, `render` (`format` + `template`), `retention` (`forever` | `days(n)` | `goal_lifetime`).

Personal default tiers: `entities`, `insights`, `archive`, `environment_knowledge` (agent, forever); `recent_activity` (agent, 30 days); `task_progress` (agent_goal, goal lifetime). No `knowledge` or `code_knowledge` default; coding workers declare `codebase_knowledge` / `architectural_knowledge` ([ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md](./ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md)).

| Default rule | Trigger | Target |
|------|---------|--------|
| `extract_entities`, `extract_insights` | `StepCompleted` | `entities`, `insights` |
| `summarize_recent_activity` | batch (min 10 episodes / 24h) | `recent_activity` |
| `update_task_progress` | `StepCompleted` | `task_progress` (`MapEpisodeToTask`) |
| `distill_insights` | batch 24h | `insights` |
| `archive_old_episodes` | batch 48h (min 20 episodes) | `archive` |
| `expire_old_episodes` | `RetentionExpiry` | `archive` |
| `extract_environment_knowledge` | batch (min 10 episodes / 24h) | `environment_knowledge` |
| `promote_to_user` | batch 72h | `user.preferences` |

Transforms: `Llm` (`MemoryConsolidationOperation`, `$ref:` prompt) or `Structured` (`BuiltinTransform`); merges `UpsertByName`, `UpsertBySimilarity`, `AppendPeriod`. Episodes keep 7 days with defaults, else 90 (`EpisodeRetention`).

Hybrid index: `{agent_runtime}/index/{manifest.json,documents.jsonl,lancedb/}`, dirtied by definition writes; `memory_utility_reviewer` labels up to 12 injected memories per run (`utility_review_queue.json`); `memory_hot_projections` persist T0 lanes. With `user_memory_isolation: shared` (default) user tiers live in `{agent_runtime}/users/` (`{tier}.json`, `profile.json`, `knowledge.json`, `work_evidence.json`, `work_entities.json`), or `agents/{id}/user_memory/` when `fully_isolated`. `GET /agents/{id}/memory/{tier}` needs `goal` for an `agent_goal` tier when several goals could apply.

---

## Approvals

**`ApprovalGate`** purely matches `ExecutionPlan` steps against `constraints.requires_approval` → `Approved` | `NeedsApproval { pending_actions }`. **`ApprovalService`** keeps durable requests in `{agent_runtime}/agents/approvals/`, driven by `StateMachineInterpreter` with the built-in `approval` machine (or a compatible `{agent_runtime}/system/state_machines.yaml`):

```
pending  --decision_approve--> validating --(plan hash valid)--> approved
                             \--(plan hash invalid)--> rejected
pending  --decision_reject--> rejected
pending|validating --ttl_elapsed--> expired
pending --delivery_failed--> rejected
```

TTL 86400s; max 4 resolve attempts; plan-hash drift is `PlanDrift`. Fan-out uses enabled `ChannelConfig`s or the in-app default; wire events are `hitl.requested` / `hitl.resolved`. `ApprovalRequest` (`approval_id`, `agent_id`, `goal_id`, `trigger_seq`, `cycle_id`, `execution_id`, `pending_actions`, `plan_hash`, `status`, `created_at`, `expires_at`, `resolved_by`/`resolved_at`) is stamped with `principal`/`workspace` so a resolve from any surface reaches the requesting scope; delivery states `sent` / `resolved` / `dismissed` / `failed`. Resolve (`/api/magician/v2/approvals`) maps to `ApprovalDecision::{Approve,Reject}`. Previews never waive or debit envelopes; the envelope system (`approval_envelopes/`) waives covered acts and is not this gate.

---

## Proposals

`ProposalStore` keeps `DefinitionProposal` (UUID, `yaml_before` / `yaml_after`; create needs non-empty `source` and a valid agent id) under `{agent_runtime}/agents/proposals/`; status `pending` | `approved` | `rejected` | `deferred`, decisions `approve` | `reject` | `defer`. Apply re-validates YAML and trust (`ProposalApplicationError`), stale transitions fail closed, and a crash between `update_definition` and `mark_applied` is recovered.

---

## Agent updates and observability

### Operator feed

`AgentUpdate` is flat JSON (envelope `id` ULID, `ts` millis, `workspace_id`, `agent_id`, optional `thread_id` / `cycle_id`, plus tagged `kind`), emitted only via `publish_agent_update` / `agent_update_envelope` (`event_type = "agent.update"`). One writer per process appends `scopes/<principal>/<workspace>/updates.jsonl` (rotates at 50 MiB; rotated files are archival). `GET /api/magician/v2/updates` tails the current file (`since`, `limit` 200 / max 1000, `agent`, `kind`, `thread`); bearer scope is authoritative (client `workspace` ignored); path-escaping ids are rejected.

`AgentUpdateKind`: `agent_created|updated|deleted|paused|resumed`, `cycle_started|completed|failed|paused`, `goal_completed|failed|recovered`, `approval_requested|resolved|expired`, `artifact_created`, `artifact_create_failed`, `circuit_opened|recovered`, `task_created|updated|completed|failed`, `memory_report`, `feedback_generated`, `tier_consolidated`, `delegation_issued|resolved`, `feed_stalled`, `published_surface_changed`.

Cycle and HITL updates are emitted inline beside `agent.cycle.*` / `hitl.*` (`agent_update_from_legacy_event` skips them). `FeedStallWatchdog` (one bus subscriber; tick 60s, GC 24h) emits `FeedStalled { silent_for_ms }` once per cycle after 600s without a terminal cycle update.

### Agentic loop events

`RuntimeTransportEvent` (`realtime_events.rs`): `AgenticExecutionStarted`, `AgenticIterationStarted`, `AgenticIterationCompleted`, `AgenticStepStuckWarning`, `AgenticPageUnderstanding`, `AgenticDecisionMade`, `AgenticActionExecuted`, `AgenticExecutionCompleted`, `AgenticWaitingForConfirmation`, `AgenticWaitingForUser`, `AgenticResumed` (`resumed_from_iteration`, `input_type`, `user_responded`). `AgenticWaitingFor*` are markers; the payload is `HitlRequested { source: "agentic" }`. See [AGENTIC_EXECUTION_DESIGN.md](./AGENTIC_EXECUTION_DESIGN.md), [FLAT_LOOP.md](./FLAT_LOOP.md).

---

## Outward gate

`outward_gate.rs` fail-closes every act leaving the machine (mail, chat, calendar, forms), independent of `ApprovalGate`: capability allowed in this work (`work_context_refusal`), contact allowed (`contact_refusal` / suppression register), dispatch acknowledged (`DispatchLog` + `DeliveryLedger::unreconciled`). Absence is refusal. Empty recipients refuse only when they mean "failed to parse who this reaches"; class-derived addressing (own calendar, site form) skips the register. Receipts: `outward_receipts.rs`.

---

## API surface

`magician-api/src/web_api.rs`, prefix `/api/magician/v2`, authenticated principal/workspace scope.

| Method | Path | Role |
|--------|------|------|
| POST / GET | `/agents` | Create (JSON/YAML) / list (`{ agents, system_agents, total_count }`, hydrates templates first) |
| POST | `/agents/refresh-definitions` | Reload scoped YAML |
| GET | `/agents/health`, `/agents/{id}/health` | Crew / agent health |
| GET / PUT / PATCH / DELETE | `/agents/{id}` | PATCH is RFC 7386 merge patch |
| GET | `/agents/{id}/harness-overview` | Harness / focus areas |
| GET | `/agents/{id}/effective-tools` | Resolved tools after tools/excluded/denied/trust/surface |
| GET / POST | `/agents/{id}/runtime-context-cache[/refresh]` | Runtime bundle / rebuild |
| POST | `/agents/{id}/set-primary` | Move `is_primary` from current primary to target |
| POST | `/agents/{id}/consolidate-user-memory` | User-promotion / shared-tier consolidation |
| POST | `/agents/{id}/trigger`, `/pause`, `/resume` | Manual cycle; durable pause / clear |
| GET | `/agents/{id}/episodes`, `/artifacts`, `/memory`, `/memory/consolidation-health`, `/memory/{tier}` | Read views |
| GET | `/triggers` | Scheduler entries |
| GET / POST | `/approvals`, `/approvals/{id}`, `/approvals/{id}/resolve` | Approvals |
| GET / POST | `/proposals`, `/proposals/{id}`, `/proposals/{id}/resolve` | Proposals |
| GET | `/updates` | Operator feed (`agent_updates_api.rs`) |
| GET / PUT | `/agents/{id}/layout` | GAUI layout (`gaui_api.rs`) |

Create emits `agent.created`, reconciles progress subscriptions and signals definition change. Not implemented: `/agents/{id}/correct`, `/export`, `/agents/import`, `/gallery/*`, `/delegations/*`, `/agents/{id}/delegations`, `/workflows/*`; delegation is `delegation_targets` + invocation policy + the executor's `DelegationDispatcher`.

### Capability catalog cache

The catalog behind `effective-tools` and `runtime-context-cache` is a snapshot keyed by a digest of the scope's `SKILL.md` files (not a timer), rechecked on a cache hit at most every 30 s per scope. A changed digest **builds and validates a candidate before swapping**, and the swap happens only if no skill the served snapshot loaded newly fails, so a half-saved `SKILL.md`, mid-checkout tree or unknown manifest field cannot drop a skill; removed skills and broken new skills do not hold it. Swaps are in place, so a scope always has a snapshot. Status: `held_back_scopes` (served/observed revisions, failing skills), `held_back_revisions`, `swaps`. Parsed packs are cached below it (`CapabilityWorkspaceManager::load_pack_defs_for_scope`) so `LocalToolServices::all_tools` does not re-parse per delegation target (that dominated spawn latency for `delegation_targets: ["*"]`); they revalidate on the same 30 s digest (scope skills root, extra skills roots, Task Recipe catalog) or `invalidate_scope_pack_defs` / `clear_scope_pack_cache`, and candidates read disk (`load_pack_defs_for_scope_fresh`), so validation never checks the served copy. `scope_pack_cache_status()`: `hits`, `revalidations`, `builds`, `invalidations`. Beneath both, `load_skills_dir` parses a skills directory once per source fingerprint (listing, contract-file contents, `bin/` and `config/.env` presence, `MAGICIAN_SKILLSHUB_ROOT`), dropped eagerly by `invalidate_scope_pack_defs` and `ScopedCapabilityResolver::clear_cache`, and chat parses a scope's trust policy once per content digest (`agents::trust::load_trust_policy_file_cached`).

---

## Capability dispatch (agents as consumers)

Tools are YAML packs run by `PackCapabilityProvider` (`execution/pack_provider.rs`, `lower_step_with_registry` in `execution/lowering.rs`). `ImplementationType`: `Composite` (capability sequence), `Compiled` (`provider_name`), `Primitive` (`native_action_schemas` promoted to flat leaves; the nested inner loop is retired, see [FLAT_LOOP.md](./FLAT_LOOP.md)), `Command` (subprocess: `program`, `fixed_args`, `arg_mappings`, …); no `JavaScript`. Native vs browser is `requires_browser_session()`. A scope's catalog (`CapabilityWorkspaceManager::load_pack_defs_for_scope_uncached`) loads, first name wins: `<scope>/skills/<skill>/tool_schema.yaml`, then each `tool-runtime-config.yaml` `registry.paths` entry, then the compiled pack defs embedded in the binary; the old `<scope>/capabilities/packs/` fallback is removed. There is no file watcher; skill changes are picked up by the 30 s digest recheck above. `POST /capabilities/reload` still exists but is a no-op (empty `ReloadReport`, `magician-bin/src/main.rs`).

---

## Related

- [UNIFIED_AGENTIC_ARCHITECTURE.md](./UNIFIED_AGENTIC_ARCHITECTURE.md), [AGENTIC_EXECUTION_DESIGN.md](./AGENTIC_EXECUTION_DESIGN.md), [FLAT_LOOP.md](./FLAT_LOOP.md), [IN_CONTEXT_DELEGATION.md](./IN_CONTEXT_DELEGATION.md) (owner hops), [ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md](./ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md), [TASK_STATE_DESIGN.md](./TASK_STATE_DESIGN.md), [USER_ESCALATION_ARCHITECTURE.md](./USER_ESCALATION_ARCHITECTURE.md) (pause/resume on exhausted failure), [storage-v2-format.md](../storage-v2-format.md)
- Historical: AGENTIC_EXECUTION_MVP.md, TRUE_AGENTS_EXECUTION_BOARD.md, TRUE_AGENTS_PHASE3.md, TRUE_AGENTS_PHASE4.md, TRUE_AGENTS_PHASE5.md
