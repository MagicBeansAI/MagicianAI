# Unified Agentic Architecture

**Scope**: Agent + Task as the live work model.

**Invariant**: two user-facing work primitives: **Task** (the executable unit) and **UI thread** (`ui_thread_id`, default `"general"`). Conversation context lives on the task and its executions (no `V2Thread`). Workflows are out of runtime scope.

Related: [TRUE_AGENTS.md](TRUE_AGENTS.md), [AGENTIC_EXECUTION_DESIGN.md](AGENTIC_EXECUTION_DESIGN.md), [FLAT_LOOP.md](FLAT_LOOP.md), [IN_CONTEXT_DELEGATION.md](IN_CONTEXT_DELEGATION.md), [LINKED_TASK_ARTIFACTS.md](LINKED_TASK_ARTIFACTS.md), [storage-v2-format.md](../storage-v2-format.md). Unbuilt autonomous-kind design: AGENTIC_AUTONOMOUS_DESIGN.md.

### Glossary

| Term | Meaning |
|------|---------|
| **Task** | Executable unit of work, assigned to an agent. V3 `TaskManifest` + `TaskState` under `scopes/<principal>/<workspace>/tasks/{id}/` (or `internal_tasks/` when `lifecycle: Internal`). |
| **Execution** | One run of a task (`ExecutionRun`). Direct work goes through `execute_agentic_direct_with_outcome`. |
| **Agent** | Declarative executor (`AgentDefinition`): identity, tools, trust, memory, invocation policy. |
| **Goal** | Cycle language, not a stored entity. Scheduling is `Task.schedule` or `AutonomousConfig` on a Personal agent. |
| **PlanGraph** | Optional stored plan (`…/tasks/{id}/plans/`), rendered into runtime context; not a second executor. |

---

## 1. Concept model

```
AGENT (who)
├── kind: Personal | Worker
├── tools / excluded_tools / denied_tools
├── delegation_targets  (empty = none; "*" = eligible in-scope targets)
├── constraints.coordination  (max_delegation_depth default 2)
├── memory, trust, invocation_policy, llm_routing
└── optional autonomous_config / harness  (Personal only)

TASK (what)
├── agent_id          (V3 create resolves an owner)
├── schedule          (optional cron / interval / once / event)
├── created_by        (string, default "user")
├── depends_on        (upstream task ids)
├── approved          (default true)
└── lifecycle         (Persistent | Internal)
```

User-facing work defaults to the workspace primary Personal agent (`is_primary`). Workers receive work only through `delegate_to_agent` / `handover_to_agent`. Goals and triggers are not on the agent definition; scheduling is `Task.schedule` (`TaskSchedule` in `storage/task_models.rs`).

---

## 2. Agent kinds

`AgentKind`: `Personal` (default) | `Worker`. There is no `Autonomous` kind.

### Personal

Entry point for user work. Validation (`types.rs`):

- `principal` + `workspace` both or neither.
- `is_primary`: exactly one per workspace (`definition_store` clears the previous one).
- May set `autonomous_config`, `harness`, `readable_agents` (agents whose memory it can read).
- Empty `tools` = all tools, unless untrusted (untrusted agents must declare a non-empty allowlist).

The runtime never fabricates a Personal agent: `trigger_goal_awaitable_with_scope_and_overrides` aborts if the definition is not in the scoped store.

### Worker

- Must declare non-empty explicit `tools` (lone `"*"` refused).
- Must not set `autonomous_config`, `harness`, `is_primary`, `readable_agents`.
- Further delegation only when `constraints.coordination.allow_transitive_delegation` (default **false**) and depth < `max_delegation_depth` (default **2**).

```
User ── creates / plans / executes a Task ──► PERSONAL AGENT (is_primary)
                                                ├── own tools → File / Http / Bash / DuckDb / Pack / browser skill
                                                ├── create_task (compiled pack) → new V3 task
                                                ├── delegate_to_agent → WORKER (child execution, or in-context hop)
                                                └── handover_to_agent → WORKER (same execution, new owner)

PERSONAL AGENT with autonomous_config
    cron / focus-area wake → same agentic loop; minted work → create_task
```

---

## 3. Agent definition

`AgentDefinition` (`magician/src/magician_v2/agents/types.rs`) is the YAML contract. Full field reference: [agent-definition-reference.md](../agents/agent-definition-reference.md). Load-bearing fields:

| Field | Role |
|-------|------|
| `agent_id`, `name`, `aliases`, `wake_spellings`, `persona`, `description` | Identity. Empty `agent_id` is slugified from `name`. |
| `kind` | `Personal` \| `Worker`. |
| `tools` | Allowlist (name or category). Empty = all (Personal). |
| `excluded_tools` | Dropped from effective / deferred / dispatch projections. |
| `denied_tools` | Hard deny; always wins. |
| `denied_tool_params` | Per-tool parameter prefix denylist. |
| `browser_transports` | Opt-in ceiling (`cdp` / `headed` / `headless`). Empty = all three. |
| `constraints` | `max_iterations` 4000, `max_tokens_per_cycle` 20_000_000, `max_consecutive_failures` 3, `requires_approval: Vec<ApprovalRule>`, `approval_ttl_secs` 86400, `coordination`, `allow_self_modification`, `max_duration_secs`. |
| `trust_level` | Trust policy key. |
| `delegation_targets` | Default empty (no delegation). `"*"` expands only if source and target `invocation_policy.delegation` are both `Wildcard`. |
| `invocation_policy` | `discoverability` (`Ambient` / `Explicit` / `SurfaceOnly`) + `delegation` (`Wildcard` / `Explicit` / `None`) + optional `allowed_direct_surfaces`. |
| `chat_inline` | `Off` / `Auto` / `Confirm` — see §10. |
| `autonomous_config` | Personal only. Cron + focus areas (§8). |
| `harness` | Personal only. Optional `program_section`. |
| `llm_routing` | Per-lane + `operations` map + optional `coding_profile` (§13). |
| `app_tool` | Typed app-facing child-task contract; absent = not an `agent_as_tool` target. |
| `disabled` | Disables this agent and reachable delegation descendants. |

`CoordinationConfig` holds `max_delegation_depth`, `delegation_timeout_secs` (legacy serialized value; live child budgets come from the call's `timeout_secs` / `depth`), `allow_transitive_delegation`.

`GoalSource` on a cycle: `User` | `Schedule` | `ChatInline`. `InvocationSourceKind` adds `Autonomous`, `Delegated`, `Handover`, `ProductFeature`, `Public`.

`constraints.requires_approval` rules match tool / action / `when.param_matches` / `url_contains`. Empty = no gate beyond trust policy.

Storage: `scopes/<principal>/<workspace>/agent_runtime/` (definitions, approvals, `scheduler_state.json`, per-agent `goal_cycles.json`).

### Invocation surfaces

`InvocationSurface` is derived from the authenticated route, never client-supplied: `Chat`, `RealtimeVoice`, `Task`, `Delegation`, `Handover`, `ThinkingMap`, `Tutor`, `AppCopilot`, `ContextualAssist`, `PublicEnvoy`, `Meeting`, `Plane`.

Empty `allowed_direct_surfaces` admits Chat, RealtimeVoice, Task, Delegation, Handover, Tutor, AppCopilot, ContextualAssist (not `ThinkingMap`, `PublicEnvoy`, `Meeting`, `Plane`). A non-empty list is an exact allowlist. Narrowing the primary agent without `chat`, `task`, and `contextual_assist` breaks Magican keyboard lanes — see [magican-keyboard.md](../../magios/magican-keyboard.md).

---

## 4. Tool scoping

- **Allowlist**: empty `tools` = all (Personal). Workers must name tools.
- **Exclude**: `excluded_tools` removes from projections.
- **Deny**: `denied_tools` is the fail-closed dispatch boundary.
- **Delegation roster**: `resolve_effective_delegation_target_ids` (and its handover variant) intersects `delegation_targets` with each target's `invocation_policy`. System agents, disabled agents, and an empty source list yield no targets.

The policy snapshot (`execution/agentic/policy_snapshot.rs`, `native_integration.rs`) applies these lists; `TrustPolicyEnforcer` is the YAML trust-policy backup. The planner uses the scoped pack surface and never widens to delegates' tools. At execution the loop sees the **active owner's** tools plus `delegate_to_agent` / `handover_to_agent` when the roster is non-empty.

Native `ExecutableAction` variants: `File`, `Http`, `Bash`, `DuckDb`, `Pack`, `SpawnSubGoal`, `DelegateToAgent`, `HandoverToAgent`, `SleepUntil`. Browser work enters through the browser skill / agent-browser loop.

Packs: embedded defs in `magician/src/magician_v2/execution/embedded_pack_defs/`; evolved packs in `CapabilityPackStore` under the scoped capability-evolution root.

---

## 5. Task model

### V3 contract

`TaskManifest` / `TaskState` (`artifact_v2/models.rs`). Routes `/api/magician/v3/tasks*` (`magician-api/src/task_api_v3.rs`).

| Field | Shape |
|-------|-------|
| `agent_id` | Required on create; resolved via `validate_task_agent_assignment`. |
| `schedule` | Optional `TaskSchedule`. |
| `created_by` | String, default `"user"` (also `"agentic_compiled"`, `"app_action"`, …). |
| `depends_on` | Upstream task ids. `reference_task_ids` on create must already be `completed`. |
| `approved` | Default `true`. |
| `lifecycle` | `Persistent` (user `/tasks`) or `Internal` (`internal_tasks/`, chat/debug/delegation side-effects). |
| `sync_mode` | `Deferred` (default) or `Await`. |
| Plan files | PlanGraph under `…/tasks/{id}/plans/`. `TaskListItemV3.has_plan` is derived from that index. |

`TaskSchedule` (`storage/task_models.rs`):

- `kind`: `Cron { expression, timezone }` \| `Interval { seconds, jitter }` \| `Once { at }` \| `OnEvent { event_pattern }`
- `missed_fire_policy`: `Skip` (default) \| `RunOnce` \| `Queue`
- `concurrent_execution_policy`: `Skip` (default) \| `Queue` \| `CancelPrevious`
- `max_runs`, `paused`, `execution_history_retention`

### Task status

`TaskStatus`: `Pending`, `Planning`, `Ready`, `Running`, `Paused`, `Completed`, `Failed`, `Cancelled`, `Deferred`. Executable: `Ready` | `Paused` | `Deferred`. Terminal: `Completed` | `Failed` | `Cancelled`. V3 stores it as a string.

---

## 6. Task lifecycle

Planning is an explicit API, not an automatic step on create:

| Action | Route |
|--------|--------|
| Create | `POST /api/magician/v3/tasks` |
| Plan | `POST …/tasks/{id}/plan` (does not run the task) |
| Approve / reject / replan | `…/plan/approve`, `…/plan/reject`, `…/plan/replan` |
| Execute | `POST …/tasks/{id}/execute` |
| Status | `PUT …/tasks/{id}/status` |

```
Pending ─ plan ─► Planning → Ready ─ execute ─► Running → Completed | Failed | Cancelled
                                                        ↘ Paused / Deferred (retry_at)
```

`create_task` (compiled pack, `execution/compiled_handlers/create_task.rs`) is how an agent mints a V3 task; it is not an `ExecutableAction`. It leaves the task `ready` when scheduled, `run: "manual"`, the kill-switch is on, or auto-dispatch depth > 3; otherwise it auto-dispatches `start_execution`.

Chat/default delegations land as `Internal` and stay off `/tasks` unless `track_as_task: true`. Chat-inline cleanup uses `TaskLifecycle` + `GoalSource::ChatInline` (see §10, [chat-mode.md](../chat-mode.md)).

---

## 7. Scheduling

Schedules live on tasks. `WakeEntry` (`agents/wake_up_queue.rs`) is keyed by `task_id`; optional `execution_id` covers execution-scoped retries (`ChildCompleted`, sleep-until). Legacy `agent_id` / `goal_id` still deserialize.

Boot hydrates due work via `ArtifactV2Service::list_scheduled_tasks_across_scopes` (skips paused and `max_runs`-exhausted schedules). `TaskSchedulerService` (`storage/task_scheduler.rs`) ticks cron/once. Concurrency policy is per task; `TaskState.schedule_fire_count` enforces `max_runs`.

Agent automation uses a second path: `AgentScheduler` registers `(agent_id, goal_id)` with optional `task_id` (`Cron` / `Event` / `Idle`). Focus-area cycles use goal id `harness:{agent_id}:{slug}`; wake ids are `scoped:{hex principal}:{hex workspace}:{hex agent}:{hex goal}`; `AgentRuntime` cancel tokens are keyed `legacy:{agent_id}:{goal_id}`.

Each admitted cycle writes an `AgentGoalRecord` (`runtime.rs`) keyed by `cycle_id` (`agent_id`, `goal_id`, `execution_id`, `goal_input_hash`, `source`, `status`) to `{agent_dir}/goal_cycles.json`.

---

## 8. Autonomous cycles

Autonomous work is `AutonomousConfig` on a Personal agent, running the same agentic loop — not a third kind and not a fixed PlanGraph.

```yaml
autonomous_config:
  schedule: "0 */4 * * *"
  max_tasks_per_cycle: 3      # default
  max_steps_per_plan: 10      # default
  focus_areas:
    - name: Morning briefing
      description: Scan sources and draft a brief
      priority: high
      schedule: "0 8 * * *"    # optional per-area override
      program: path/to/program.md
      scope: ["*"]
```

`build_autonomous_goal` (`agents/autonomous_goal.rs`) assembles persona + matching focus areas + memory tiers. The cycle calls `execute_agentic_direct_with_outcome` with `max_spawned_tasks = max_tasks_per_cycle`, labelled `InvocationSourceKind::Autonomous`. New work goes through `create_task`.

---

## 9. Delegation

Delegation is a runtime decision inside the flat loop, not a PlanGraph step kind. Tools (`execution/agentic/native_catalog.rs`, lowered in `native_lowering.rs`):

| Tool | Behavior |
|------|----------|
| `delegate_to_agent` | `DelegateToAgent`. Fan-out array `delegation_targets`; each child is a task-backed execution unless the chat in-context single-target path applies. Returns when **enqueued**, not complete. |
| `handover_to_agent` | `HandoverToAgent`. Permanent owner transfer on the same execution. |
| `spawn_sub_goal` | `SpawnSubGoal`. Same-agent nested loop run inline in the parent execution (no new task); requires `unblocks`. |
| `create_task` | Compiled pack; mints a V3 task. |

`DelegationTargetRequest`: `target_agent_id`, `context`, optional `input_artifact_ids`, `input_data`, `depth`, `timeout_secs`, `spend_token_ids`, `required_capability`, `expected_artifacts`, plus catalog-only `track_as_task` / `personality_mode` / `reference_task_ids`. No `share_session` — for session continuity the catalog tells the model to call owned tools instead of delegating.

Safety gates:

- Source `delegation_targets` × target `invocation_policy`.
- `max_delegation_depth` 2; `allow_transitive_delegation` false by default.
- Disabling a parent disables reachable descendants.
- `constraints.requires_approval` rules.
- Browser transport ceiling is per agent, so a child cannot inherit CDP through the parent carrier.

Chat `orchestrate_pipeline` single-target delegates can run **in-context** on one `ExecutionRun` (owner swap, no child). Multi-target and non-pipeline delegations spawn children. See [IN_CONTEXT_DELEGATION.md](IN_CONTEXT_DELEGATION.md).

Each agent keeps its own memory tiers; consolidators run on cycle completion. Child results reach the parent as `## RECENT DELEGATION RESULTS` / child deliverables; there is no post-delegation memory-collection LLM.

---

## 10. Chat inline

`chat_inline` on the **target** agent:

| Policy | Behavior |
|--------|----------|
| `None` / `Off` | Not available for inline `delegate_to_agent` (default). |
| `Auto` | Runs without confirmation. |
| `Confirm` | User confirms before the cycle starts. |

Inline runs use `GoalSource::ChatInline`: an ephemeral task-backed execution in the caller's scope. Successful outputs are preserved and the backing task deleted; failed/cancelled runs archive. `track_as_task: true` keeps the task on `/tasks`.

---

## 11. Cross-task artifact linking

At execution start the orchestrator (`v2_orchestrator.rs`, STEP 5c) reads `depends_on`, reuses pinned `linked_task_inputs` if present, otherwise resolves upstream outputs via V3 refs, and injects a read-only `## LINKED TASK ARTIFACTS` section into `prior_environment_knowledge` (~4000 char cap).

Completion names: `durable:namespace/name` (file in `DurableArtifactStore`) or `inline:name|content_type|preview`.

`DurableArtifactStore` (`artifacts/durable_store.rs`): workspace-scoped files; crash-safe write (temp + `sync_all` + rename), `fs2` locks, YAML frontmatter with `source_task_id`, `resolve_path_safe()` rejects `..`, absolute paths, null bytes. `SpawnSubGoal` children share the parent's `ActionExecutors`, so durable writes roll up on `GoalReached`.

Full contract: [LINKED_TASK_ARTIFACTS.md](LINKED_TASK_ARTIFACTS.md).

### Per-step `step_hash`

`PlanStep` (`strategy/plan.rs`) stores `providing_agent_id` and `step_hash = hash(providing_agent_id + tool_id)` (16-hex). `None` is valid (legacy plans). Before replay, a changed agent/tool invalidates only that step.

---

## 12. Execution

Both paths end in `execute_agentic_direct_with_outcome(execution_id, goal, max_iterations, trust_context, overrides)`; a stored PlanGraph is rendered into runtime context first. `AgenticContext.depth` starts at 0.

The pipeline (`IntentClassifier` / `Elicitor` / `Planner`, `TieredRouter`) serves only the **Plan** action. Delegated children and scheduled re-runs execute without a user click but never skip trust/approval gates.

| Path | When | Engine |
|------|------|--------|
| Plan | `POST …/plan` | Pipeline stores a `PlanGraph`; list items report `has_plan`. |
| Do, no plan | `POST …/execute` or `create_task` auto-dispatch | Direct runtime from title/description + success criteria. |
| Do, with plan | Execute after approval | Graph rendered into context; `step_hash` checked, only invalid steps replanned. |
| Chat / Veil / channel | Direct run, usually `Internal` | Same engine. |

Monitors are scheduled V3 tasks with `monitor_spec`; `/api/magician/v3/monitors*` wraps the task service and pause/resume flips `TaskSchedule.paused`. See [recurring-monitors.md](../recurring-monitors.md).

---

## 13. LLM routing

```yaml
llm_routing:
  planning:
    profile: opus47-messages-toolsany-rnone
  evaluation:
    provider: openai
    model: gpt-5.6-terra
  operations:
    agentic_decision:
      profile: sonnet46-messages-toolsany-rnone
  coding_profile: coding-premium
```

Lanes: `planning`, `evaluation`, `correction_extraction`, `memory_consolidation`. Precedence: request override → agent `operations` / lane → global `llm.router.operation_mapping`. Profiles live in `llm-router.yaml` (spliced into `llm.router` at load).

---

## 14. What did not ship

Do not reason from these original-design claims:

- `AgentKind::Autonomous` with a fixed observe/process/analyze PlanGraph → Personal `autonomous_config` on the shared loop.
- `capability_packs` / `excluded_packs` / `delegate_to` → `tools` / `excluded_tools` / `denied_tools` / `delegation_targets`.
- Removing `CoordinationConfig`, depth 3, Personal `delegate_to: ["*"]` → config kept, depth 2, empty targets, transitive off.
- OEO `delegate` / `create_task` actions, `share_session`, post-delegation memory LLM, `archive_on_terminal`, startup "rerunnable" rewrite, `capability_templates/packs/` seeds, combined own ∪ delegate planner catalog → none exist.

---

## Related

- Agents runtime: [TRUE_AGENTS.md](TRUE_AGENTS.md)
- Loop, actions, yield: [AGENTIC_EXECUTION_DESIGN.md](AGENTIC_EXECUTION_DESIGN.md), [FLAT_LOOP.md](FLAT_LOOP.md), [YIELD_DECISION.md](YIELD_DECISION.md)
- V3 layout: [storage-v2-format.md](../storage-v2-format.md); REST: [v2-api-guide.md](../v2-api-guide.md)
