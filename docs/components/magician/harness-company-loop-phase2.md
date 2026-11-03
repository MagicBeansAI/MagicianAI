# Team routines — backlog, roles and cycle inputs

**Work → More → Team routines** controls the shared company loop. This contract
covers its backlog, product-owner role (CPO) and live cycle inputs, building on the [Phase 1 detect-and-fix loop](./harness-detect-fix-loop.md).
Design: Autonomous Company Loop.

## Flow

```
any harness agent ── propose_backlog_item ──▶ programs/backlog/<id>.json
                                                    │
CTO / CPO cycle prompt ◀── "### Company Backlog (open)" (append_harness_program_context)
                       ◀── "### Repo State"  (branch, dirty count, last 5 commits)
                                                    │
CPO / CEO ── promote_backlog_item ──▶ create_task(assignee) + start_execution(task) + mark item Promoted
                                                    │
assignee (e.g. CTO) tiers + staffs the build in the started V3 task
                                                    │
terminal task ── inspect_backlog_delivery + review_backlog_delivery ──▶ Delivered or Proposed (rework)
```

## Testing Cadence

Seed definitions: CRO `5,35 * * * *`; CMO `10,40 * * * *`; CPO backlog
grooming `15,45 * * * *` and product strategy review `50 */2 * * *`; CTO
standup `20,50 * * * *` (deeper reviews hourly or every two hours); CEO
briefing `25,55 * * * *` (wrap/review lanes buffered hourly or every two
hours): ~5 minutes between stages, ~30 minutes between each agent's primary
runs. Scheduler reconciliation picks up a cron change only after the runtime
defs under `MAGICIAN_ROOT_DIR` are synced with the seed templates.

`/harness` has a `Start team sequence` shortcut that calls the normal manual
trigger API (not a scheduler bypass) once per focus area of each harness agent
(a definition with a `harness` block), read from the live definitions in
agent-id order, and reports accepted/queued/failed per stage. An agent with no
focus areas contributes no stage. `POST /agents/{id}/trigger` rejects a
`harness:{agent}:` goal id that names none of the agent's current focus areas
(`invalid_goal_id`), also when the agent has none left, so a retired routine
can no longer run without its program.

## Backlog store

`harness::BacklogStore` (`harness/backlog.rs` + `harness/backlog_store.rs`)
persists one `programs/backlog/<id>.json` per directive under the scope
(`ArtifactV2Workspace::programs_backlog_path`), deduped by
`id = "bk_" + sha256(source_agent ∅ title)[..12]` so re-proposing the same
title updates in place. `BacklogItem { id, principal, workspace,
source_agent, title, description, priority (High|Medium|Low), status
(Proposed|Promoted|Delivered|Dismissed), created_at, updated_at,
promoted_task_id, promotion_task_ids, delivery_review }`. `list` sorts
`Proposed`-first, then by priority, then newest-first; a missing dir reads
as empty; writes are atomic. `promotion_task_ids` preserves every attempt
across rework while `promoted_task_id` points at the current attempt.

## Action tools

Four harness tools, wired via the 4-file pattern (`harness/mod.rs` name
const + `HARNESS_ACTION_TOOL_NAMES`/`HARNESS_TOOL_NAMES`;
`embedded_pack_defs/<name>.yaml`; `COMPILED_PROVIDERS` +
`embedded_compiled_pack_source_table()` in `compiled_providers.rs`; dispatch arm +
`execute_*` in `harness_provider.rs`):

- **`propose_backlog_item`** (`title`, `description`, `priority?`) — any
  harness agent appends a prioritized directive. Priority is case-insensitive.
- **`promote_backlog_item`** (`backlog_item_id`, `agent_id`, `goal_id?`,
  `start_immediately?`) — CPO/CEO converts a groomed item into an assigned
  task via `ArtifactV2Service::create_task` (item priority + provenance
  note), starts it by default with `start_execution`, then marks `Promoted`.
  `agent_id` is **required and scope-validated**
  (`HarnessScope::require_contains`). Ordering is get → create_task →
  start_execution → mark. Pass `start_immediately: false` only to park the
  promoted task for manual start.
- **`review_backlog_delivery`** (`backlog_item_id`, `disposition`,
  `summary`, `revised_description?`) — accept a completed delivery or
  return the same item to `Proposed` for rework. Acceptance is rejected
  unless the linked task is `completed`.
- **`inspect_backlog_delivery`** (`backlog_item_id`) — bounded read packet:
  requested outcome, status, output previews, persisted artifact metadata,
  and material evidence refs. Tool discovery and control captures are
  non-material. Acceptance fails closed when no material persisted delivery
  evidence exists.

Harness-created task descriptions carry a delivery contract: materialize the
requested outcome, never cite planned artifacts as proof, and yield
partial/blocked while work remains. The executor requires a substantive
successful action and excludes tool-discovery/control captures from completion
evidence. The coding engine cannot call these tools; they run only inside a
harness cycle.

## Real cycle inputs

`append_harness_program_context` (`magician-api/src/web_api.rs`), right
after the Phase-1 `### Anomalies caught (open)` block, appends three
best-effort blocks to every harness agent's cycle prompt:

- **`### Harness Health`** — scheduler inventory, due/missed cron, open
  anomalies, `FixDispatched` repairs, and promoted backlog items missing
  task linkage or awaiting delivery review (completed tasks stay visible
  until `inspect_backlog_delivery` then `review_backlog_delivery`;
  failed/ready outcomes route to rework). Relay (`harness-sre`) uses it as
  primary input before delegating forensics to Sonar. Recurring dispatched
  anomalies reopen with dispatch metadata kept, so cooldowns still suppress
  duplicate repair tasks.
- **`### Company Backlog (open)`** — open (`Proposed`) backlog, top 8, one
  line per item (`[priority] title — proposed by {agent} (id …)`).
- **`### Repo State`** — current branch, uncommitted-file count, last 5
  commit subjects, from the **located live repo only**
  (`coding_engine::live_repo_source_fence`; `None` skips the block).

A missing store, git absence, timeout, or list error is skipped or surfaced
as a compact unavailable line and never blocks the cycle.

## CPO and CMO

**CPO (Nova)** —
`magician_data_v3/system/agent_templates/agents/cpo/definition.agent.yaml`.
A `personal` harness agent (`harness.program_section: product`) that grooms
the backlog and promotes ready product work to engineering **via the CTO**.
Delegates to `cto`/`web-researcher`/`company-assistant`; added to the CEO's
`delegation_targets`. Focus areas: *backlog grooming* (`product_ops.md`) and
*product strategy review* (`product_strategy.md`). **CMO (Echo)** marketing
lane is enabled.

Seed agent defs and CEO delegation live in the repo; the CPO runtime def and
its program docs live under `MAGICIAN_ROOT_DIR` and must exist before the
CPO's first cycle.

## Phase 2.5 — harness action-tool dispatch + owner-approval gate

Harness action tools dispatch through the **per-scope** capability registry
(`ScopedCapabilityResolver::registry_for_scope` → `build_compiled_registry`).

- **Dispatch:** `register_harness_action_providers_if_absent` binds the
  full `HARNESS_ACTION_TOOL_NAMES` set per-scope (`if_absent`, never
  clobbering a handler-backed provider like `create_task`).
- **Owner-approval gate:** `HARNESS_APPROVAL_GATED_TOOL_NAMES` =
  `create_agent`, `update_agent`, `retire_agent`, `update_delegation` route
  through the executor's `ApprovalGate`. `update_program_state` is ungated.
  `agents::approval::harness_mutation_approval_rules` builds one
  `{ tool, action: "*" }` rule per gated tool;
  `harness_merged_approval_rules(&definition)` merges them into
  `requires_approval` (deduped; a narrow YAML rule can't suppress the
  central `*` gate) **when `definition.harness.is_some()`**. Applied at
  every site that builds a harness agent's execution `approval_rules`:
  manual + scheduled dispatch (`magician-api/src/web_api.rs`),
  `load_owner_execution_profile` (`orchestrator/v2_orchestrator.rs`),
  `delegate_to_agent` (`agents/runtime.rs`), and chat binding
  (`chat/service.rs`).
- **Effect:** roster/program self-mutation pauses as owner-approvable HITL
  (`HitlRequested` → `/feed/attention` + durable `ApprovalRequest`).
  `create_task`/`reassign_task`/`create_proposal`/`create_dashboard`/
  `update_program_state`/the backlog tools dispatch **ungated**.
- **Spawn cap:** `autonomous_config.max_tasks_per_cycle` (default 3) rides
  into the cycle as `__max_spawned_tasks` and is enforced inside `create_task`
  (`compiled_handlers/create_task.rs`) by counting persisted tasks tagged with
  the source execution/task provenance.
- **Scheduled coalescing:** a cron dispatch drops when the same
  scope/agent/goal already has active or queued work (manual owner runs can
  still queue). Scheduled root dispatches share a process-wide capacity limit
  (`try_admit_harness_root`); a cron fire over it is dropped and recorded, so
  restart catch-up cannot launch every due harness agent at once.

The envoy diode (`ask_owner` / `request_owner_action` / `propose_meeting`,
`compiled_handlers/owner_relay.rs`) is inbound and stranger-facing only;
`notify_owner` only informs. A gated real action is an agent-definition
edit: grant the outbound tool and add a `requires_approval` rule. The CMO
carries `agentmail-send` with `action: "*"` (flat pack derives
`(tool="agentmail-send", action="execute")`; a narrower `send` would miss
the outer `execute` token). The gate pauses before the inner send loop
until the owner approves in `/feed/attention`.

Global kill-switch: `harness.paused` / `MAGICIAN_HARNESS_PAUSED` blocks
scheduled and manual harness starts and the steward/autofix dispatchers;
Crew control also drains queued starts and requests active-cycle
cancellation (see [detect-fix loop config](./harness-detect-fix-loop.md#config)).

## Definition-driven gating

`magician_v2/harness/registration.rs` owns what a `harness:` block implies:

- **Tool gate.** `harness_tool_grant_for_definition` — personal agents with
  a `harness:` block ride the `Coordinator` grant (`HARNESS_TOOL_NAMES` plus
  `notify_owner`); everyone else gets harness tools stripped.
  `v2_orchestrator::visible_tools_for_definition` calls the registration.
- **Reflection hook.** `reflects_program_runtime_state` decides whether
  terminal-execution learning reflection enriches context with the harness
  program document + runtime state; `artifact_v2/service.rs` calls it
  instead of testing `harness.is_some()` inline.

The officer roster
(`magician_data_v3/system/agent_templates/agents/{ceo,cto,cmo,cpo,cro,harness-sre}`)
and scope-seeded programs (e.g. `programs/harness_reliability.md`) opt in
through `kind: personal` + a `harness:` block. `company-assistant` is
`kind: worker` and receives no harness tool grant. Harness stores
(episode/trace/backlog/anomaly/program state) stay core Layer 1.

## Key files

`harness/{backlog,backlog_store,mod,registration}.rs`;
`execution/embedded_pack_defs/{propose,promote}_backlog_item.yaml` and
`{inspect,review}_backlog_delivery.yaml`;
`execution/{harness_provider,compiled_providers,scoped_capability_resolver}.rs`;
`agents/approval.rs`; `magician-api/src/web_api.rs`
(`append_harness_program_context`, `harness_repo_state_block` /
`run_harness_git`);
`magician_data_v3/system/agent_templates/agents/cpo/definition.agent.yaml`.
