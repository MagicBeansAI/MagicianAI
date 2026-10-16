# Execution Documentation

Living contract for Magician execution: launch/recovery, the flat agentic loop,
dispatch, delegation, synthesis, spend, and native tool calling. Core loop:
[AGENTIC_EXECUTION_DESIGN.md](AGENTIC_EXECUTION_DESIGN.md). Sibling specs and
archived boards are indexed at the end.

## Stateless accepted-launch and recovery contract

- **Envelope.** Direct message execution, explicit starts and
  create-with-initial-message (direct and planned) persist a scope-HMAC-sealed
  **accepted-launch envelope** before HTTP 202 (dormant Runtime generation,
  Artifact/root/owner identities, mode, message, bounded options; renewable
  `Pending -> Claimed -> Consumed`). Its intent is composition authority through
  `Planning` / `PlanningComplete` and the pre-seed / first-loop
  `Runnable`/`Executing` crash window.
- **Recovery.** The paginated candidate scan verifies dormant rows' envelopes
  as a prefilter; active rows need
  Artifact's typed exact/pre-seed base authority; a dormant shell without an
  intent stays dormant. Planned recovery reuses a deterministic inbound turn id
  (no duplicate user message). A live exact loop lease at startup gets
  AlreadyOwned plus a recheck at lease expiry
  (`schedule_accepted_runtime_launch_claim_recheck`), so a dead owner's segment
  becomes RecoverableExact instead of hanging. Legacy/taskless PlanGraph
  executions converge via their retained durable plan and typed base-execution
  authority; startup neither closes them for lacking an Artifact task nor
  invents intent. Sealed scopes classify in index-only batches of ≤20,000 base
  executions; an unsealed oversized scope gets one bounded discovery and stays
  fail-closed.
- **Retirement.** A returned launch future does not consume the envelope; that
  waits for a typed waiting/terminal Runtime state or a positive
  live/exact/settlement-pending handoff. Startup GCs ≤20,000 Consumed envelopes
  per scope (only exact-path, HMAC-valid entries still Consumed after a fenced
  re-read).
- **Task-addressed wakes** stay leased while handlers run, renew the exact
  generation and ack only after successful handoff; consumers reserve handler
  capacity before leasing and cap claims to those permits, so no row waits
  unstarted behind the local semaphore. Shutdown joins claimed
  handlers and quiesces the execution-job registry before event/storage sinks.
- **Parent settlement** needs exact child-roster, Runtime, Artifact, lineage and
  result-summary evidence, then one atomic store op makes the parent runnable.
  Terminal receipt recovery uses a bounded checksummed candidate catalog,
  falling back to authoritative journal/receipt paging when it is dirty, corrupt, stale, incomplete or overflowed.
- **Fail closed.** Composition and terminal projection fail on any scope,
  identity, lineage, output, media, tool-result or manifest storage error; only
  typed not-found is absence, and I/O, decode or corruption never silently
  disables guardrails or parent/resume behavior.

Cutover to stateless per deployment: restart on the new binary, drain legacy
writers, publish each scope's immutable seal; `GET /health/execution-driver`
then reports `stateless`.

## Coding-engine structural regression contracts

- Coding-engine JSON depth admission counts nested arrays/objects, not scalars,
  iteratively: hostile nesting is bounded, shallow wide results pass.
- Source-shape regression tests inspect only a module's production portion;
  `#[cfg(test)]` strings cannot fake or duplicate a production API.
- `publish_social_post` is retired; agents reach Town Square through the
  `town-square` package's `ambient_turn` behavior (app-platform scheduled and
  budgeted). `compiled_providers` pins that the name no longer resolves, so a stale grant
  falls through.

## Planning, HITL, iteration ceilings, and sandbox escalation

- The panel's **Plan** action starts task-scoped planning without running the
  task; questions are answered in
  the Plan tab (Attention is an alternate surface). Event tails are
  reconnecting HTTP streams; `BodyStreamBuffer was aborted` is not a failure.
- Named agent templates and the runtime default use a 4,000-iteration ceiling
  (resuming an older lower-ceiling pause upgrades it); context-heavy named
  agents use a 20M execution-token ceiling during development (live
  definitions, system templates and scoped seeds agree). Duration,
  consecutive-failure and concurrency controls stay active.
- Shell sandbox violations and native file paths outside configured roots enter
  resumable HITL; approval adds only the requested absolute paths to that
  execution's in-memory policy, never global config, and never bypasses the
  coding source-tree hard fence.
- Terminal failures stay in task state, run history and observability, not
  Attention (no operator decision resolves them).

## Bootstrap and capability-readiness latency contract

- Bootstrap and HITL resume never run capability probes (`auth.check_command`,
  `reliability.preflight.probe`): the merged catalog is an authority ceiling and
  readiness comes from the real invocation (probes can start CLIs, hit
  OAuth/network or touch a keychain). Setup/diagnostic surfaces may use
  `execution/agentic/preflight.rs`.
- Capability snapshot cache: O(1) warm lookup; skill/config mutation
  invalidates the scope; a coalesced revision check runs ≤ every 30s per active
  scope; `refresh_scope` forces it. Cold builds hash before and after, so a
  racing mutation cannot publish an incoherent registry/index pair.
- Prompt-context hydration has a **500 ms** budget for the first decision and
  HITL resume (`agent_surface_runtime.context_retrieval.autonomous_task.deadline_ms`); memory and procedure retrieval run concurrently and late branches
  are dropped (resume still advances its semantic checkpoint).
- Timing record (ends before the loop): `bootstrap_total_ms`,
  `owner_profile_ms`, `prompt_context_ms`, `execution_context_ms`,
  `capability_snapshot_ms`, `runtime_state_ms`, `is_resume`; over 1,000 ms logs
  a warning, otherwise debug.
- Exact resume reuses a checkpoint's frozen initial tool scope. Pause records
  hold the live message sequence or the legacy rendered summary, not both.
- The per-turn scheduler copy holds only prompt fields (URL/title/stage,
  visible and accessibility text at 2,000/3,000-byte prompt forms, screenshot
  lookup/fallback, viewport; FS/HTTP/shell fields bounded); Merkle nodes,
  element maps, overlays and diagnostics stay in live state; historical
  environment bodies are released once summarized; a complete disk-backed
  screenshot skips a second base64 copy. Delegation results render once, in
  decision-scoped reconciliation.
- Child terminal handling settles or resumes the parent **before** episode
  extraction, memory materialization or progress projection; notifications go
  only after the committed answer is projected. Deferred progress refreshes coalesce per
  task (a generation change guarantees a final one); settlement replay clears
  stale internal delegation checkpoints; the terminal-auxiliary exactly-once
  marker covers continuation/media/episode only.
- Child reducers reload the child inside the task lock and apply only their own
  status/output mutation (clocks keep the newest committed timestamp); Runtime signals
  refuse terminal → non-terminal for children and root tasks.

## Explicit named delegation and exact continuation

`POST /api/magician/v3/tasks/{task_id}/execute` accepts optional
`delegate_to_agent`: a typed first owner transition (not prompt injection) that
skips only the root LLM turn. Policy, target availability/disabled state, depth/cycles,
idempotency, admission and child isolation still apply; empty targets and
self-delegation fail; the route is stored with the execution so a restart keeps
it. Depth counts the inherited chain
(`delegation_chain ∪ owner_stack ∪ {active owner}`, minus the active owner);
handover and in-context hops count toward `max_delegation_depth`; a root with
nothing suspended is depth 0.

`delegate_to_agent` fans out over *different* targets; it does not run N agents
on one problem. **Admission collapses equivalent work**:
`delegation_idempotency_execution_id` (`agents/runtime.rs`) folds context,
sorted/deduped input artifact ids, spend tokens and expected artifacts into the child
id. **Reconciliation aggregates; it never chooses** (delegation-results prompt in
`execution/agentic/decision.rs`).

| Limit | Default | Override |
|---|---|---|
| Live loops (`admit_agent_loop`) | hard cap 50, optional RSS tripwire | `configure_live_agent_limit(0)` = observe-only |
| Per-agent outstanding (counts a root waiting on children: each open VibeDev run holds a CTO slot) | 8 (`current`/`small_cpu`), 16 `m2_max`, 32 `cloud_heavy` | `runtime.scale.overrides.per_agent_outstanding_limit` (`0` disables), at boot via `configure_per_agent_outstanding_limit` |
| Delegated-child permits, held for the child's whole run | `DELEGATED_CHILD_PERMITS_BY_LEVEL` `[4, 4, 2, 1]` (level 0 = child of a root) | `runtime.scale.overrides.delegated_child_permits` raises levels 0–1 at boot (`configure_delegated_child_permits`; 4, 8 `m2_max`, 16 `cloud_heavy`; never below 4) |

Per-level pools keep a full fan from deadlocking. A VibeDev coding child holds a
level-0 permit for its whole engine turn, so these and
`coding.trusted_max_concurrent_runs` bound parallel VibeDev runs. A blocked
child logs `[DELEGATION] child waiting for a
delegated-child slot`. Beyond `DELEGATED_CHILD_LEVELS`, acquisition waits at
most `DEEP_DELEGATION_MAX_WAIT` (120 s), then `force_child_failure`. Admission is
serialized per parent.

- **Routing overrides** are tree-wide: children merge the parent route and get
  a durable sidecar before launch, write-once under the task's cross-process
  lock with a scope-keyed HMAC seal (principal, workspace, task, execution).
  Every resume verifies seal and pause snapshot; edits, removal or copying to
  another execution fail closed (a deleted sidecar leaves an orphan seal that
  blocks reload). Owner defaults and the immutable execution overlay are stored
  apart (hot reload replaces only defaults); legacy pauses freeze their prior
  effective route rather than widening authority; pre-seal sidecars stay
  readable and are sealed only when the identical launch route is re-presented
  under the task lock.
- **Internal delegation checkpoint**: a yielding parent stores compacted
  conversation, environment knowledge, provider checkpoint/image cohort,
  iteration/budget, owner/trust and tool authority (hidden from
  HITL/Attention/WebSocket pause reads);
  terminal children resume it with only their result evidence.
- **Typed one-child launch**: after a successful child the parent projects the sole accepted child output
  (exact source execution and path), commits output refs and completes with no
  model summary or reconciliation; V3
  commit precedes the V2 status/child-group settle in one write; retries join
  the same reconciliation lock; zero or
  multiple accepted outputs fail the parent.
- **Launch ordering**: child registration writes the V3 row, route and parent
  link once; schedule/feed/progress are derived after launch and never hold
  admission; root sidecars are
  durable before a running execution is exposed (a failure leaves an
  unreferenced shell, never a running task without a worker).
- **Timing events**: `ExecutionRuntimeHandoff` / `DelegationLaunchDispatched`
  (launch), `ExecutionRuntimeResumeHandoff` (resume), `ExecutionProgress`
  (answer + parent projection). Summary, indexing, media, schedule and episode
  work is post-answer and never controls completion; startup retries it.
  Liveness uses the full internal pause index, so a recoverable parent is not
  classified dead.

A delegated coding child that stages a Pi diff raises `diff_approval` HITL
(`WaitingUser`; Attention `source` `"diff_approval"`) while the parent stays
`WaitingChildren` (`delegated_waiting_for_user_should_suspend`);
`cancel_execution_tree` spares a cascaded child parked on that pause; explicit
root cancel still terminates the root.

## Operation-routed LLM calls route through the dispatch queue

Agentic and operation-routed calls go through `OperationLlmRouter`
(`query_analysis/operation_llm_router.rs`) into the global `LlmDispatchQueue`
(priority lanes, retry, circuit breakers, idempotency, local-prep;
[llm-dispatch-queue.md](../llm-dispatch-queue.md);
design).
Cut sites: `generate_via_router`, `dispatch_execution_native_messages`,
`generate_multimodal_via_router`. Installed at boot
(`magician-bin/src/main.rs`, `set_dispatch_queue`) over the same
`ConfiguredRouter`; `llm.dispatch.enabled` (default true) false routes
directly. Priority (`priority_for_operation`): voice and meeting response →
High; memory, workflow/recipe compilation, app operations, screen observation,
meeting summary and named evidence/channel maintenance ops → Background; else
Normal. Ollama prewarm needs `llm.dispatch.local_prep.enabled`.
`LlmQueuePanel` polls `/api/llm/queue/snapshot`.

- **Streaming** `generate_chat_completion_streaming*`: `submit_stream`, High
  lane, chat-trace `TaskRef` (so `cancel_chat_session` tombstones queued
  streams); unwired → `ConfiguredRouter::route_stream` + `llm_dispatch_bypass`.
  Non-streaming helpers stay direct.
- **Cancellation**: loop jobs carry `TaskRef::task(execution_id)`;
  `cancel_execution` → `CancelBridge::on_task_cancelled` → `queue.cancel_task` +
  `task_state_view.fire_cancel` (per node in `cancel_execution_tree`); the worker
  snapshot gate passes unresolvable runtime ids; the token is subscribed before
  `mark_in_flight`. Chat turn: `DELETE /api/magician/v2/chat/sessions/{id}/run`.
- **Telemetry**: `analytics/llm_dispatch_rows.rs`, one Parquet row per terminal
  `LlmQueueEvent` at `<default-scope>/analytics/llm_dispatch/dt=*` (lane,
  provider, retries, tombstone reason, queue-wait vs provider time, tokens,
  local-prep savings), even with dispatch disabled.
- **Local-prep**: memory consolidation passes `(raw, purpose)`; a
  `SummarisableBlock` is carved only when enabled (default off =
  byte-identical prompt). Agentic `Text` outputs over 4,000 bytes become a typed
  history omission, not a summary. Truncated structured responses (`max_tokens` / `length` /
  token `incomplete`) fail closed after one bounded non-stream retry with a larger output budget.

## Browser session handoff via the `yield` tool

Terminal `yield` on browser runs takes `keep_browser_window_open` (detach CDP,
leave Chromium open) and `keep_browser_cdp_connection_alive` (keep CDP + daemon
for reattach; alias `keep_browser_session_alive`; wins if both).
`primitive_dispatch/cleanup.rs::cleanup_browser_session_if_done` ORs these with
`ctx.keep_browser_*` to skip cleanup, run `agent-browser close --keep-browser`,
or close fully. See [`YIELD_DECISION.md`](YIELD_DECISION.md),
`2026-05-24-browser-session-lifecycle-redesign.md`.

## Execution-panel `run.activity_log`

`ExecutionPanelRunState.activity_log` (`execution_panel/types.rs`): the full
seq-ordered (oldest→newest) humanized event log of the selected execution, from
`build_activity_log` (`execution_panel/v3_adapter.rs`) for the deep-work feed,
on the full-state panel delta; distinct from `recent_activity` (capped 6,
newest-first).

## Tool dispatch: flat loop, compiled vs interpreted

No per-tool inner-loop LLM: the outer loop sees flat `<pack>__<action>` leaves
and dispatches LLM-lessly (`execution/primitive_dispatch/`; pack schema
`type: primitive`, legacy alias `inner_loop`); chat capability-pack dispatch is
a flat agentic sub-execution. See [`FLAT_LOOP.md`](./FLAT_LOOP.md),
`2026-05-29-flat-loop-phase7-retire-inner-loop.md`.
Every work tool flows `lower_pack_execute` → `ExecutableAction::Pack` →
`CapabilityRegistry::execute`, then by `ImplementationType`:

- **Compiled:** `FileCapabilityProvider` / `HttpCapabilityProvider` /
  `DuckDbCapabilityProvider`, or `GenericCompiledProvider` over
  `compiled_handlers/<name>::handle` (`media_edit` / `media_edit_status` use an
  internal `MediaOp` registry, not one tool per ffmpeg op;
  design).
  Registration is three entries in `compiled_providers.rs`: (1) YAML in
  `embedded_pack_defs/` + `include_str!` in
  `embedded_compiled_pack_source_table()`, (2) `registry.register(...)` in
  `default_compiled_handler_registry()`, (3) `COMPILED_PROVIDERS`. Missing (3)
  still builds, but `prune_unexecutable_pack_defs` silently drops the pack at
  boot; `build_compiled_registry` binds any deferred pack with a registered
  handler through `GenericCompiledProvider` (no hand-written block).
- **Interpreted:** `Composite` (`execute_composite`), `Command`
  (`CommandArgMapping` argv), `Primitive` (`native_action_schemas` promoted to
  leaves, dispatched via compiled provider / CLI template / agent-browser).

Both share catalog, tool-call shape and a `Value` result. Control tools stay on
native lowering (`execution/agentic/native_lowering.rs`). `yield`
is the sole LLM-emitted terminal (`goal_reached` / `cannot_proceed` are compat
aliases; `Decision::Completed` / `Failed` are synthetic fallbacks;
yield-unification).

## Persona / Skill / Tool three-tier prompt composition

| Tier | Owner | Mutable per execution? |
|---|---|---|
| **Persona** | `definition.agent.yaml::persona` (identity, decision rules, safety, scope, reporting) | No — frozen at start |
| **Tool selection** | Each pack `description` (`## AVAILABLE CAPABILITIES`) | Per-execution catalog |
| **Procedure skill** | `skillshub/<slug>/SKILL.md` body (`## ACTIVE PROCEDURE PLAYBOOK` when active) | Yes — `activate_skill` / `deactivate_skill` |

A runtime fourth tier (artifact-output guidance, plus dashboard guidance when
`create_dashboard` is in the catalog) comes from
`decision.rs::build_artifact_output_guidance_section` into
`{artifact_guidance_section}`. Chat delegates get persona via
`AgenticContextOverrides.prompt_identity`. `activate_skill` /
`deactivate_skill` are outer-loop control tools, offered when ≥1 procedure skill
resolves and not in chat mode
(`skills::active::resolve_and_activate_procedure_skill`);
`AgenticContext.active_procedure_skill` is per-execution and cleared on owner
transition. Agent-facing slugs: `cua-driver` → `macos-ui-automation`,
`mac-automation-simple` → `macos-script-automation`. Design:
`2026-05-25-persona-skill-architecture.md`.

## Output synthesis

`finalize_terminal_execution` runs 1.1 `execution_output`, then 1.3
`task_user_output` on
`task_projection_execution_id = root_execution_id.unwrap_or(self)`. 1.3 is
**root-gated**: a child reuses 1.1 as placeholder and feeds the parent via
`child_output_refs` → `source_output_ids`. `MAGICIAN_TASK_SYNTHESIS_ROOT_GATE=0`
restores per-execution task synthesis.

There is **no agent-audience task output**: the 1.2 `task_agent_output` stage
(op `v3_task_agent_output_synthesis`, file `out_task_agent_*`) was retired
2026-10-01;
agents read the user output. The task-user finalizer starts from
`execution_output`: a first Markdown/plain-text (or verified JSON) run projects
its bytes through the grounding boundary, otherwise it synthesizes with it as
predecessor (also the grounding fallback). Legacy: `default_task_agent_output_id`
deserializes and is cleared on the next terminal commit; overwrite mode prunes
a stale `primary_task_agent` ref; `synthesis_failed.stage = "task_agent_output"`
loads as `task_user_output`; `existing_finalized_outputs` needs only execution
and user outputs; `task_agent_finalizer.*` names remain for old logs, unemitted.

`persist_execution_outcome` never blocks on LLM synthesis:

- **Step 1 (sync):** `commit_multi_write_journal_path` flips terminal +
  `synthesis_pending` and adds the id to
  `task.state.synthesis_pending_executions`; outputs, episodes and
  dep-scheduler projection are untouched.
- **Step 2 (`tokio::spawn`):** `spawn_synthesis_pipeline_for_existing` (write
  path, startup reconciler, retry) reloads and runs 1.1–1.3 via
  `retry_synthesis_step` (3 attempts, 1s → 2s). Ok → `reduce_execution_terminal`;
  retry exhaustion → `synthesis_failed`; other errors →
  `reduce_execution_terminal_without_outputs`; panics persist a
  `SynthesisFailure`. It writes state/refs only (no manifest), so late synthesis
  cannot clobber rename/retag/repriority.
- **Read gate** (`artifact_v2/read_gate.rs`): `OutputReadOutcome<T> = Ready(T) |
  Pending | Failed`; `gated_execution_read` skips the loader unless `Ready`.
- **Retry:** `POST /api/magician/v3/tasks/{task_id}/executions/{execution_id}/retry-synthesis`,
  idempotent (pending and not failed → `{ coalesced: true }`);
  `synthesis_failed_execution_id` points the UI at the latest failure.

Known gaps: retries mint a fresh `output_id`; two processes on one scope can
both pass Step 1. Design:
`2026-05-24-async-output-synthesis.md`.

Chat `delegate_to_agent` returns `{status: "enqueued"}` (never claims success);
`spawn_delegate_teardown_watcher` waits for terminal + synthesis readiness, then
emits `chat.delegate.output_ready` / `output_failed`. Delegate/handover fanout
times out at 3h with synthetic failure; `subscribe_chat_to_task` at 24h,
silently.

## Resource authority

`resource_authority.enabled` is `true` in `magician-config.yaml`; API and spend
journal: [resource-authority-api.md](../resource-authority-api.md).

- `create_task` / `create_agent` are not gated (`MaybeGatedAction::Bare`).
- Spend comes only from pack-def `execution.spend.commodity`
  (`pack_provider.rs`); agents cannot declare a gate and config never binds a
  tool on its own. Spend packs (tool-scoped): `agentmail-send` (`EMAIL_SENDS`),
  `kapso-whatsapp-send` (`WHATSAPP_SENDS`). An `agent` / `id: "*"` / `USD` row
  is a shared pool.
- Every path admits just before provider I/O: CLI-template primitives via
  `maybe_wrap_with_spend_gate` → `execute_maybe_gated`, app compiled owners on
  prepared dispatch, OS-jail skills from `runtime_catalog.spend`, compiled paths
  through `execution::compiled_dispatch` (`execute_direct` refuses `Gated`).
- `execute_maybe_gated` **fails open (uncounted)** with a `warn!` when no
  `budgets:` row matches `(commodity, scope/id)`; exhaustion, ceiling and freeze
  still reject. Orthogonal to owner consent (`ApprovalGate`); `period: daily`
  re-funds each UTC day.

**INR checkout (Zepto/Swiggy)**: governed MCP
(`primitive_dispatch/governed_mcp.rs`) runs `spend_session::admit` (fail-closed)
on the skill's `commerce.final_tools`, gating only
`risk == checkout_or_payment` calls; the agent's `order_amount_inr` must match
the live cart. Knobs sit in the skill's `runtime_contract` `commerce:` block
(e.g. `skillshub/swiggy-mcp/SKILL.md`), not env: `cart_amount_unit`, optional
`max_order_minor`, tolerances. Flow: per-order ceiling → live-cart check →
tool-scoped `INR` reservation → final call → `commit` / `rollback`; an
ambiguous post-dispatch transport error commits and reports
`ambiguous_transport`, so nothing retries the checkout. Missing identity fails
checkout. `allow_cartless_checkout` / `cartless_endpoint_aliases` and
`allow_unverified_cart` are separate, so a cartless flow never weakens a real
cart.

**REST** (`magician-api/src/resource_authority_api.rs`):
`POST /resource-authority/reservations` funds via the dispatch
`SpendTokenResolver`, then `gate::reserve_spend` (`409` on ceiling or missing
row); commit takes optional `actual`; the workspace-bound bearer selects the
ledger.

## Native tool calling

Autonomous decisions use provider-native tool calls; the catalog alone defines
names and argument shapes (no parallel envelope in the prompt). Autonomous zero-tool responses are errors; chat may
call one tool or return text. The runtime instruction adds only what schemas
cannot carry (≥1 autonomous call, ordered non-terminal batching, terminal
placement, evidence-backed completion); `FOCUSED CAPABILITY ROUTING` only when
the planner picked a preferred tool or another agent owns it. Procedure-skill
and artifact guidance are prompt context, not schema. Prompt pins:
[AGENTIC_EXECUTION_DESIGN.md](AGENTIC_EXECUTION_DESIGN.md); static check:
`scripts/eval-agentic-native-tool-contract.sh`.

`system:*` agents are never delegation targets, planner workers or tool
providers; the scheduler is an internal wake queue. Browser work uses the
`browser` skill (pinned `agent-browser` CLI); Magicutor is only CDP proxy / extension bridge / API-mining sidecar (no
`/execute` action JSON).

## Runtime contracts

- **Execution runtime** (`magician-bin/src/main.rs`,
  `execution/runtime_boundary.rs`): dedicated Tokio runtime, 4 workers on the
  `current` profile, 2 `small_cpu`, 8 `m2_max` / `cloud_heavy`
  (`runtime_plan.rs`) ([boundaries](../runtime-async-stack-boundaries.md)).
- **V3 HTTP** (`magician-api/src/task_api_v3.rs`) shares V2 CORS/`OPTIONS`.
  Cancel and artifact reads are execution-scoped (`/executions/{id}/cancel`,
  `/executions/{execution_id}/artifacts/{artifact_path:.*}`); planning is
  task-scoped (`/tasks/{id}/plan`, `/execution-panel`).
- **Planning admission:** `start_task_planning` holds the per-task write lock
  only for journal recovery, validation, initial `Planning` persist and status
  write (not feed/progress, transport or planner spawn).
- **Exact resume:** `resume_execution_tree()` only; manual continue fails closed
  unless the target is durably paused and resumable.
- **Pause store:** `k2_<hash>.json` keyed by `storage_key()`; legacy hex names
  stay readable; hashed + legacy for one key fails loudly.
  `AgenticWaitingForUser` carries the full `UserInputType` schema; direct pauses
  resume through `/execution/agentic-resume`.
- **Planner catalog** (`magician-bin/src/adapters/local_tool_services.rs`)
  resolves the scoped pack surface once. **Linked inputs:** `depends_on`
  validated at create; root start pins `linked_task_inputs`; orchestrator injects `## LINKED TASK ARTIFACTS`.
- **Downloads:** `…/executions/<execution_id>/artifacts/downloads/` with
  `…/artifacts/persisted_artifacts.json`; pipeline snapshots at
  `…/pipeline/store.json`.
- Spatial surfaces are observation data, not a canvas planner
  (historical board).
  Events: [v2 websocket events](../v2-websocket-events.md). Chat:
  [routing](../chat-profile-routing.md), [streaming](../chat-sse-streaming.md).

## Skill-bundled binaries (`{skill_runtime_root}/bin/...`)

Vendored binaries live in the skill `bin/` (e.g.
`skillshub/metabase/bin/metabase-pp-cli`). `make skills-install-scope
SCOPE=<principal>/<workspace>` links them into
`$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<skill>/bin/`; the
dispatcher prepends `<MAGICIAN_SKILL_DIR>/bin` to `PATH`; absolute paths use
`{skill_runtime_root}/bin/<binary>`. See
[capabilities README](../../capabilities/README.md#metabase-pack).

## Canonical References

- **[UNIFIED_AGENTIC_ARCHITECTURE.md](UNIFIED_AGENTIC_ARCHITECTURE.md)** — Agent + Task model, kinds, OEO loop, `delegate_to` allowlist, pack scoping
- **[TRUE_AGENTS.md](TRUE_AGENTS.md)** — Agent runtime, memory, delegation, scheduling, observability
- **[AGENTIC_EXECUTION_DESIGN.md](AGENTIC_EXECUTION_DESIGN.md)** — Core loop, dispatch, budgets, loop detection
- **[FLAT_LOOP.md](FLAT_LOOP.md)** — Flat per-action loop, hot/deferred catalog, `tool_search` (old inner loop: INNER_LOOP_RUNTIME_CONTEXT.md)
- **[IN_CONTEXT_DELEGATION.md](IN_CONTEXT_DELEGATION.md)** — Single-execution owner hops for chat `orchestrate_pipeline`
- **[YIELD_DECISION.md](YIELD_DECISION.md)** — Unified terminal tool
- **[GENERATIVE_AGENT_UI.md](GENERATIVE_AGENT_UI.md)** — GAUI rendering and lifecycle
- **[LINKED_TASK_ARTIFACTS.md](LINKED_TASK_ARTIFACTS.md)** — Cross-task artifact reuse
- **[DURABLE_ARTIFACT_TAG_INDEX.md](DURABLE_ARTIFACT_TAG_INDEX.md)** — Task/agent/execution lookup over durable artifacts
- **[ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md](ENVIRONMENT_KNOWLEDGE_ARCHITECTURE.md)** — Agent-scoped site/API/tool heuristics
- **[USER_ESCALATION_ARCHITECTURE.md](USER_ESCALATION_ARCHITECTURE.md)** — Pause-and-resume for exhausted failures
- **[TASK_STATE_DESIGN.md](TASK_STATE_DESIGN.md)** — Durable agent-writable state
- **[CROSS_LAYER_CONSTANTS.md](CROSS_LAYER_CONSTANTS.md)** — Rust↔JS constant sync
- **[TREASURER_AGENT_DESIGN.md](TREASURER_AGENT_DESIGN.md)** — Treasurer spend/ledger design
- **Delegation V2 Ownership Transfer Design** — Execution-owned delegation/handover
- **Thread-to-Task Consolidation Design** — Task/execution split
- **[storage-v2-format.md](../storage-v2-format.md)** — Task + execution storage contract

## Archive

- [archive/](archive/README.md) — Local index of retained historical notes
- [future_proposals/](future_proposals/README.md) — Speculative designs not yet scheduled
- p5.5 execution / planning / PlanGraph — Historical pipeline; PlanGraph schema source of truth: `magician/src/magician_v2/strategy/plan.rs`
- AGENTIC_AUTONOMOUS_DESIGN.md — Autonomous execution design
- TRUE_AGENTS_EXECUTION_BOARD.md — Retired True Agents sprint board
- CANVAS_MODE_EXECUTION_BOARD.md — Retired canvas-specialist tracker
- LEGACY_DELEGATED_CHILD_EXECUTION.md — Historical delegated-child-thread model
- PROMPT_SYSTEM_AUDIT_2026-03-02.md — Prompt audit snapshot
