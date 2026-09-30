# Execution Documentation

Living contract for Magician execution: launch/recovery, the flat agentic loop,
dispatch, delegation, synthesis, spend, and native tool calling. Core loop:
[AGENTIC_EXECUTION_DESIGN.md](AGENTIC_EXECUTION_DESIGN.md). Sibling specs and
archived boards are indexed at the end.

## Stateless accepted-launch and recovery contract

- Direct message execution, explicit starts, and create-with-initial-message
  (direct and planned) persist a scope-HMAC-sealed **accepted-launch envelope**
  before returning HTTP 202. It binds the dormant Runtime generation,
  Artifact/root/owner identities, mode, message, and bounded options, with a
  renewable `Pending -> Claimed -> Consumed` lifecycle.
- The sealed intent is composition authority through `Planning` /
  `PlanningComplete` and the pre-seed / first-loop `Runnable`/`Executing` crash
  window. The paginated candidate scan verifies dormant rows' envelopes as a
  prefilter; active rows need Artifact's typed exact/pre-seed base authority. A
  dormant shell without an intent stays dormant. Planned recovery reuses a
  deterministic inbound turn id so the user message is not appended twice.
- A returned launch future does not consume the envelope. Retirement waits for a
  typed waiting/terminal Runtime state or a positive live/exact/settlement-pending
  loop handoff. Startup GCs at most 20,000 Consumed envelopes per scope, deleting
  only exact-path, HMAC-valid entries still Consumed after a fenced re-read.
- A live exact loop lease found at startup may belong to the just-killed process:
  recovery answers AlreadyOwned and schedules a recheck at lease expiry
  (`schedule_accepted_runtime_launch_claim_recheck`); a dead owner's segment then
  becomes RecoverableExact and resumes. Why: otherwise nothing revisits the
  execution and it hangs after the lease expires.
- Legacy/taskless interrupted PlanGraph executions converge through their
  retained durable plan and typed base-execution authority; startup neither
  closes them for lacking an Artifact task nor invents intent for them.
- Active-run recovery for a sealed scope is classified in index-only batches of
  ≤20,000 base executions. An unsealed oversized scope gets one bounded
  compatibility discovery and stays fail-closed.
- Task-addressed wakes stay leased while handlers run, renew the exact
  generation, and ack only after successful handoff. Wake consumers reserve
  handler capacity before leasing and cap claims to those permits, so no row
  waits unstarted behind the local semaphore. Shutdown joins claimed handlers and
  quiesces the execution-job registry before event/storage sinks are torn down.
- Parent delegation settlement requires exact child-roster, Runtime, Artifact,
  lineage, and result-summary evidence before one atomic store op makes the parent
  runnable. Terminal receipt recovery uses a bounded checksummed candidate
  catalog, falling back to authoritative journal/receipt paging whenever that
  accelerator is dirty, corrupt, stale, incomplete, or overflowed.
- Stateless composition and terminal projection fail closed on scope, identity,
  lineage, output, media, tool-result, or manifest storage errors. Only typed
  not-found is absence; guardrails and parent/resume behavior are never silently
  disabled by I/O, decode, or corruption.

Cutover to stateless per deployment requires restarting on the new binary,
draining every legacy writer, publishing each scope's immutable seal, and
`GET /health/execution-driver` reporting `stateless`.

## Coding-engine structural regression contracts

- Coding-engine JSON depth admission counts nested arrays/objects, not scalar
  children; traversal is iterative, so hostile nesting is bounded without
  rejecting shallow wide results.
- Source-shape regression tests inspect only the production portion of a Rust
  module; `#[cfg(test)]` strings cannot fake or duplicate a production API.
- `publish_social_post` is retired; agents reach Town Square through the
  `town-square` package's `ambient_turn` behavior (scheduled and budgeted by the
  app platform). `compiled_providers` pins that the name no longer resolves, so a
  stale grant falls through.

## Planning, HITL, iteration ceilings, and sandbox escalation

- The panel's **Plan** action starts task-scoped planning without running the
  task; planning questions are answered in the Plan tab (Attention is an
  alternate HITL surface).
- Planning/run event tails are long-lived HTTP streams that reconnect; browser
  stream cancellation (`BodyStreamBuffer was aborted`) is connection lifecycle,
  not execution failure.
- Named agent templates and the runtime default use a 4,000-iteration ceiling;
  resuming a pause from an older lower-ceiling definition upgrades the remaining
  ceiling. Context-heavy named agents use a 20M execution-token ceiling during
  development (live definitions, system templates, and scoped seeds agree).
  Duration, consecutive-failure, and concurrency controls stay active.
- Shell sandbox violations and native file paths outside configured roots enter
  resumable HITL. File approval adds only the requested absolute paths to that
  execution's in-memory policy; it never mutates global config or bypasses the
  coding source-tree hard fence.
- Terminal failures stay in task state, run history, and observability but are
  not projected into Attention (no operator decision can resolve them).

## Bootstrap and capability-readiness latency contract

- Bootstrap and HITL resume never run capability probes (`auth.check_command`,
  `reliability.preflight.probe`). The merged catalog is an authority ceiling, not
  a usage prediction, and probes can start CLIs, hit OAuth/network, or touch a
  keychain. Readiness is discovered from the real invocation; setup/diagnostic
  surfaces may use `execution/agentic/preflight.rs`.
- Capability snapshot cache: warm lookup is O(1). Skill/config mutation
  invalidates the scope; a coalesced background revision check runs at most every
  30s per active scope; `refresh_scope` forces visibility. Cold construction
  hashes before and after the build so a racing mutation cannot publish an
  incoherent registry/index pair.
- Prompt-context hydration has a **500 ms** critical-path budget for the first
  decision and HITL resume; memory and procedure retrieval run concurrently and
  late branches are omitted. Resume still advances its semantic checkpoint.
- Timing record: `bootstrap_total_ms`, `owner_profile_ms`, `prompt_context_ms`,
  `execution_context_ms`, `capability_snapshot_ms`, `runtime_state_ms`,
  `is_resume`; ≥1,000 ms logs a warning. Timing ends before the loop starts.
- Exact resume does not re-resolve the tool catalog when the checkpoint carries
  a frozen initial tool scope. Pause records store either the live message
  sequence or the legacy rendered summary, not both.
- The per-turn scheduler copy is prompt-focused (URL/title/stage, visible and
  accessibility text at 2,000/3,000-byte prompt forms, screenshot
  lookup/fallback, viewport). Merkle nodes, element maps, overlays, and
  diagnostics stay in live state; FS/HTTP/shell fields are bounded to prompt
  limits; historical environment bodies are released after action effects are
  summarized; a complete disk-backed screenshot route skips a second base64 copy.
  Delegation results render once in decision-scoped reconciliation.
- Child terminal handling settles or resumes the parent **before** episode
  extraction, memory materialization, or progress projection; notifications
  dispatch only after the committed answer is projected. Deferred progress
  refreshes coalesce per task (a generation change guarantees one final refresh).
  Settlement replay clears stale internal delegation checkpoints. The
  terminal-auxiliary exactly-once marker covers continuation/media/episode only.
- Child reducers reload the child inside the task lock and apply only their own
  status/output mutation; clocks keep the newest committed timestamp. Runtime
  signals refuse terminal → non-terminal transitions for children and root tasks.

## Explicit named delegation and exact continuation

`POST /api/magician/v3/tasks/{task_id}/execute` accepts optional
`delegate_to_agent`: a typed first owner transition, not a prompt injection.
Policy, target availability/disabled state, depth/cycles, idempotency, admission,
and child isolation are still enforced; only the root LLM turn is skipped. Empty
targets and self-delegation fail. The route is stored beside the execution so a
restart cannot turn it back into an ordinary root run.

Depth is counted over the chain a child would inherit
(`delegation_chain ∪ owner_stack ∪ {active owner}`, minus the active owner).
Handover and in-context hops count toward `max_delegation_depth`. A root with
nothing suspended is depth 0.

`delegate_to_agent` fans out over *different* targets and reconciles outputs; it
does not run N agents on one problem:

- **Admission collapses equivalent work.** `delegation_idempotency_execution_id`
  (`agents/runtime.rs`) canonicalizes context, sorted/deduped input artifact ids,
  spend tokens, and expected artifacts into the child execution id.
- **Reconciliation aggregates; it never chooses** (delegation-results prompt in
  `execution/agentic/decision.rs`).

Live-loop admission (`admit_agent_loop`): hard cap 50, per-agent outstanding 8,
optional RSS tripwire; `configure_live_agent_limit(0)` is observe-only.

Delegated children use `DELEGATED_CHILD_PERMITS_BY_LEVEL` — one semaphore per
level `[4, 4, 2, 1]` (level 0 = child of a root). A child holds its permit for
its whole run; per-level pools keep waits ordered so a full fan cannot deadlock.
Beyond `DELEGATED_CHILD_LEVELS`, acquisition waits at most
`DEEP_DELEGATION_MAX_WAIT` and over-deep work fails via `force_child_failure`.
Admission is serialized per parent, not process-wide.

**Routing overrides** are tree-wide: children merge the parent route and get a
durable route sidecar before launch; owner defaults and the immutable execution
overlay are stored separately (hot reload replaces only defaults). Legacy pauses
freeze their prior effective route rather than widening authority. The sidecar
is write-once under the task's cross-process lock with a scope-keyed HMAC seal
bound to principal, workspace, task, and execution. Every resume verifies seal
and pause snapshot; edits, removal, or copying to another execution fail closed;
deleting the public sidecar leaves an orphan seal that blocks reload. Pre-seal
sidecars stay readable and are sealed only when the identical launch route is
re-presented under the task lock.

**Internal delegation checkpoint**: when a parent yields to children the runtime
records compacted conversation, environment knowledge, provider
checkpoint/image cohort, iteration/budget, owner/trust, and tool authority
(hidden from HITL/Attention/WebSocket pause reads). Terminal children resume it
with only the new child-result evidence.

**Typed one-child launch**: after a successful child the parent projects the
sole accepted child output (read by exact source execution and path), commits
output refs, and completes — no model summary or reconciliation. V3 output/task
commit lands before the V2 parent status and child-group settle in one write;
retries join the same reconciliation lock; zero or multiple accepted outputs
fail the parent.

Child registration writes the V3 row, inherited route, and parent link once;
schedule/feed/progress are derived after launch and never hold admission. Root
launch sidecars are durable before the reducer exposes a running execution (a
failure leaves an unreferenced shell, not a running task without a worker).

Timing events: `ExecutionRuntimeHandoff` / `DelegationLaunchDispatched` (launch
stages), `ExecutionRuntimeResumeHandoff` (checkpoint resume), `ExecutionProgress`
(answer commit + parent projection). Task summary, continuation indexing, media
registration, schedule refresh, and episode recording are post-answer and never
control completion; startup retries missing auxiliary work. Orphan/liveness
reconciliation uses the full internal pause index so a recoverable parent is not
classified dead.

A delegated coding child that stages a Pi diff raises `diff_approval` HITL
(`WaitingUser`) while the parent stays `WaitingChildren`
(`delegated_waiting_for_user_should_suspend`). `cancel_execution_tree` spares a
cascaded child parked on that pause; explicit root cancel still terminates the
root. Attention `source` is `"diff_approval"`.

## Operation-routed LLM calls route through the dispatch queue

Agentic and operation-routed LLM calls go through `OperationLlmRouter`
(`query_analysis/operation_llm_router.rs`) into the global `LlmDispatchQueue`
(priority lanes, retry, circuit breakers, idempotency, local-prep). Cut sites:
`generate_via_router`, `dispatch_execution_native_messages`,
`generate_multimodal_via_router`. The queue handle is installed at boot
(`magician-bin/src/main.rs`, `set_dispatch_queue`) from the same
`ConfiguredRouter` it wraps. `llm.dispatch.enabled` (default true) falls back to
direct routing. Priority: Voice → High; Memory/WorkflowCompilation → Background;
else Normal. Local-prep Ollama prewarm only when `llm.dispatch.local_prep.enabled`.
`LlmQueuePanel` polls `/api/llm/queue/snapshot`. Design:
`2026-05-30-llm-dispatch-queue-completion.md`.

Streaming `generate_chat_completion_streaming*` uses `submit_stream` on the High
lane with a `TaskRef` from the chat trace so `cancel_chat_session` can tombstone
queued streams; unwired, it uses `ConfiguredRouter::route_stream` and records
`llm_dispatch_bypass`. Non-streaming helpers stay direct.

**Cancellation.** Loop jobs are tagged `TaskRef::task(execution_id)`;
`cancel_execution` fires `CancelBridge::on_task_cancelled` → `queue.cancel_task`
plus `task_state_view.fire_cancel` (inherited per node in `cancel_execution_tree`).
The ref is a runtime execution id, so the worker snapshot gate must proceed for
unresolvable ids. The cancel token is subscribed before `mark_in_flight`. Chat
turn cancel: `DELETE /api/magician/v2/chat/sessions/{id}/run`.

**Telemetry.** `analytics/llm_dispatch_rows.rs` writes one Parquet row per
terminal `LlmQueueEvent` to `<default-scope>/analytics/llm_dispatch/dt=*` (lane,
provider, retries, tombstone reason, queue-wait vs provider time, tokens,
local-prep savings), regardless of `llm.dispatch.enabled`.

**Local-prep.** Memory consolidation passes a `(raw, purpose)` pair; the router
carves a `SummarisableBlock` only when local-prep is enabled (default off =
byte-identical prompt). Agentic `Text` outputs over 4,000 bytes become a typed
history omission, not a summary. Structured parsing fails closed on truncated
responses (`max_tokens` / `length` / token `incomplete`) after one bounded
non-stream retry with a larger output budget.

## Browser session handoff via the `yield` tool

Terminal `yield` accepts two optional booleans on browser runs:
`keep_browser_window_open` (detach CDP, leave Chromium open) and
`keep_browser_cdp_connection_alive` (keep CDP + daemon for reattach; alias
`keep_browser_session_alive`; wins if both set). The Yield handler sets overrides
that `primitive_dispatch/cleanup.rs::cleanup_browser_session_if_done` ORs with
`ctx.keep_browser_*` to choose skip cleanup, `agent-browser close --keep-browser`,
or full close. See [`YIELD_DECISION.md`](YIELD_DECISION.md) and
`2026-05-24-browser-session-lifecycle-redesign.md`.

## Execution-panel `run.activity_log`

`ExecutionPanelRunState.activity_log` (`execution_panel/types.rs`) is the full
seq-ordered (oldest→newest) humanized event log for the selected execution,
built by `build_activity_log` (`execution_panel/v3_adapter.rs`) for the
deep-work feed; distinct from `recent_activity` (capped 6, newest-first). Ships
on the full-state panel delta.

## Tool dispatch: flat loop, compiled vs interpreted

There is no per-tool inner-loop LLM: the outer loop sees flat `<pack>__<action>`
leaves and dispatches LLM-lessly (`execution/primitive_dispatch/`; pack schema
`type: primitive`, legacy alias `inner_loop`). Chat capability-pack dispatch is a
flat agentic sub-execution. See [`FLAT_LOOP.md`](./FLAT_LOOP.md) and
`2026-05-29-flat-loop-phase7-retire-inner-loop.md`.

Every work tool flows `lower_pack_execute` → `ExecutableAction::Pack` →
`CapabilityRegistry::execute`, then by `ImplementationType`:

- **Compiled:** `FileCapabilityProvider` / `HttpCapabilityProvider` /
  `DuckDbCapabilityProvider`, or `GenericCompiledProvider` over
  `compiled_handlers/<name>::handle`. `media_edit` / `media_edit_status` use an
  internal `MediaOp` registry instead of one tool per ffmpeg op
  (design).
  Registration is exactly three entries in `compiled_providers.rs`: (1) YAML in
  `embedded_pack_defs/` + `include_str!` in `embedded_compiled_pack_source_table()`,
  (2) `registry.register(...)` in `default_compiled_handler_registry()`,
  (3) `COMPILED_PROVIDERS`. Missing (3) does not fail the build —
  `prune_unexecutable_pack_defs` silently drops the pack at boot.
  `build_compiled_registry` binds every deferred pack with a registered handler
  through `GenericCompiledProvider`, so no hand-written block is needed.
- **Interpreted:** `Composite` (`execute_composite`), `Command`
  (`CommandArgMapping` argv), `Primitive` (`native_action_schemas` promoted to
  leaves, dispatched via compiled provider / CLI template / agent-browser).

Both share catalog, tool-call shape, and a `Value`-returning contract. Control
tools stay on native lowering (`execution/agentic/native_lowering.rs`). `yield`
is the sole LLM-emitted terminal (`goal_reached` / `cannot_proceed` are compat
aliases); `Decision::Completed` / `Failed` are synthetic fallbacks. See
yield-unification.

## Persona / Skill / Tool three-tier prompt composition

| Tier | Owner | Mutable per execution? |
|---|---|---|
| **Persona** | `definition.agent.yaml::persona` (identity, decision rules, safety, scope, reporting) | No — frozen at start |
| **Tool selection** | Each pack `description` (`## AVAILABLE CAPABILITIES`) | Per-execution catalog |
| **Procedure skill** | `skillshub/<slug>/SKILL.md` body (`## ACTIVE PROCEDURE PLAYBOOK` when active) | Yes — `activate_skill` / `deactivate_skill` |

A runtime-injected fourth tier (artifact-output guidance, plus dashboard
guidance when `create_dashboard` is in the catalog) is rendered by
`decision.rs::build_artifact_output_guidance_section` into
`{artifact_guidance_section}`.

Chat delegates receive persona via `AgenticContextOverrides.prompt_identity`.
`activate_skill` / `deactivate_skill` are outer-loop control tools, offered when
≥1 procedure skill resolves and not in chat mode
(`skills::active::resolve_and_activate_procedure_skill`).
`AgenticContext.active_procedure_skill` is per-execution and cleared on owner
transition. Agent-facing slugs: `cua-driver` → `macos-ui-automation`,
`mac-automation-simple` → `macos-script-automation`. Design:
`2026-05-25-persona-skill-architecture.md`.

## Output synthesis

`finalize_terminal_execution` runs 1.1 `execution_output`, then 1.2
`task_agent_output` + 1.3 `task_user_output` on
`task_projection_execution_id = root_execution_id.unwrap_or(self)`. 1.2/1.3 are
**root-gated**; a child reuses 1.1 as placeholders and feeds the parent via
`child_output_refs` → `source_output_ids`. `MAGICIAN_TASK_SYNTHESIS_ROOT_GATE=0`
restores per-execution task synthesis.

`persist_execution_outcome` never blocks on LLM synthesis:

- **Step 1 (sync):** `commit_multi_write_journal_path` flips terminal +
  `synthesis_pending` and adds the id to `task.state.synthesis_pending_executions`;
  outputs, episodes, and dep-scheduler projection are untouched.
- **Step 2 (`tokio::spawn`):** `spawn_synthesis_pipeline_for_existing` (write
  path, startup reconciler, retry) reloads and runs 1.1–1.3 via
  `retry_synthesis_step` (3 attempts, 1s → 2s). Ok → `reduce_execution_terminal`;
  retry exhaustion → `synthesis_failed`; other errors →
  `reduce_execution_terminal_without_outputs`; panics persist a `SynthesisFailure`.
  Step 2 writes state/refs only (no manifest) so late synthesis cannot clobber
  rename/retag/repriority.
- **Read gate** (`artifact_v2/read_gate.rs`): `OutputReadOutcome<T> = Ready(T) |
  Pending | Failed`; `gated_execution_read` skips the loader unless `Ready`.
- **Retry:** `POST /api/magician/v3/tasks/{task_id}/executions/{execution_id}/retry-synthesis`,
  idempotent (pending and not failed → `{ coalesced: true }`).
  `synthesis_failed_execution_id` points the UI at the latest failure.

Known gaps: retries mint a fresh `output_id`; two processes on one scope can both
pass Step 1. Design:
`2026-05-24-async-output-synthesis.md`.

Chat `delegate_to_agent` returns `{status: "enqueued"}` (never claims success).
`spawn_delegate_teardown_watcher` waits for terminal + synthesis readiness, then
emits `chat.delegate.output_ready` / `output_failed`. Delegate/handover fanout
has a 3h timeout with synthetic failure; `subscribe_chat_to_task` has 24h and
returns silently.

## Resource authority

`resource_authority.enabled` is `true` in `magician-config.yaml`.

- `create_task` / `create_agent` are not gated (`MaybeGatedAction::Bare`).
- Spend comes only from pack-def `execution.spend.commodity`
  (`pack_provider.rs`); agents cannot declare a gate and config never binds a tool
  independently. Current spend packs: `agentmail-send` (`EMAIL_SENDS`) and
  `kapso-whatsapp-send` (`WHATSAPP_SENDS`), tool-scoped. An `agent` / `id: "*"` /
  `USD` row is a shared pool.
- CLI-template primitives with spend lower via `maybe_wrap_with_spend_gate` →
  `execute_maybe_gated`; app compiled owners keep pack spend on prepared
  dispatch; OS-jail skills read `runtime_catalog.spend`. All admit immediately
  before provider I/O. Compiled paths go through `execution::compiled_dispatch`;
  `execute_direct` refuses `Gated` actions.

`execute_maybe_gated` **fails open (uncounted)** when no `budgets:` row matches
`(commodity, scope/id)`, logging `warn!`; exhaustion, ceiling, and freeze still
reject. Orthogonal to owner consent (`ApprovalGate`). `period: daily` re-funds
each UTC day.

**INR checkout (Zepto/Swiggy)** runs through governed MCP
(`primitive_dispatch/governed_mcp.rs`) and `spend_session::admit` (fail-closed)
on the skill's declared `commerce.final_tools`; only `risk ==
checkout_or_payment` calls are gated. The agent's `order_amount_inr` must match
the live cart. Knobs live in the skill's `runtime_contract` `commerce:` block
(e.g. `skillshub/swiggy-mcp/SKILL.md`), not env vars: `cart_amount_unit`,
optional `max_order_minor`, tolerances. Flow: per-order ceiling → live-cart
cross-check → tool-scoped `INR` reservation → final call → `commit` /
`rollback` (an ambiguous post-dispatch transport error commits and reports
`ambiguous_transport` so nothing retries the checkout). Missing identity fails
checkout. `allow_cartless_checkout` / `cartless_endpoint_aliases` and
`allow_unverified_cart` are separate so a cartless flow never weakens a real
cart.

**REST** (`magician-api/src/resource_authority_api.rs`, bound to 127.0.0.1):
`POST /resource-authority/reservations` funds via the same `SpendTokenResolver`
as dispatch, then `gate::reserve_spend` (`409` on ceiling or missing row); commit
takes optional `actual`. The workspace-bound bearer selects the ledger.

## Native tool calling

Autonomous decisions use provider-native tool calls; the catalog is the sole
authority for names and argument shapes, with no parallel envelope in the prompt.
Autonomous zero-tool responses are errors; chat may call one tool or return text.
The runtime instruction adds only what schemas cannot carry: at least one
autonomous call, ordered non-terminal batching, terminal placement,
evidence-backed completion. `FOCUSED CAPABILITY ROUTING` is added only when the
planner picked a preferred tool or another agent owns it. Procedure-skill and
artifact guidance are prompt context, not invocation schema. Details and prompt
pins: [AGENTIC_EXECUTION_DESIGN.md](AGENTIC_EXECUTION_DESIGN.md).

Static contract check: `scripts/eval-agentic-native-tool-contract.sh`.
`system:*` agents are never delegation targets, planner workers, or tool
providers; the scheduler is an internal wake queue. Browser work uses the
`browser` skill (pinned `agent-browser` CLI); Magicutor is only CDP proxy /
extension bridge / API-mining sidecar (no `/execute` action JSON).

## Runtime contracts

- **Execution runtime** (`magician-bin/src/main.rs`,
  `execution/runtime_boundary.rs`): dedicated four-worker Tokio runtime.
  [Runtime async stack boundaries](../runtime-async-stack-boundaries.md).
- **V3 HTTP** (`magician-api/src/task_api_v3.rs`): shares V2 CORS/`OPTIONS`.
  Cancel and artifact reads are execution-scoped (`/executions/{id}/cancel`,
  `/executions/{execution_id}/artifacts/{artifact_path:.*}`); planning is
  task-scoped (`/tasks/{id}/plan`, `/execution-panel`).
- **Planning admission:** `start_task_planning` holds the per-task write lock
  only for journal recovery, validation, initial `Planning` persist, and status
  write — not across feed/progress, transport, or planner spawn.
- **Exact resume:** `resume_execution_tree()` only; manual continue fails closed
  unless the target is durably paused and resumable.
- **Pause store:** files `k2_<hash>.json` keyed by `storage_key()`; legacy hex
  names stay readable; hashed + legacy for one key fails loudly.
  `AgenticWaitingForUser` carries the full `UserInputType` schema; direct pauses
  resume through `/execution/agentic-resume`.
- **Planner catalog** (`magician-bin/src/adapters/local_tool_services.rs`):
  resolve the scoped pack surface once.
- **Linked inputs:** `depends_on` validated at create; root start pins
  `linked_task_inputs`; orchestrator injects `## LINKED TASK ARTIFACTS`.
- **Downloads** stage under `…/executions/<execution_id>/artifacts/downloads/`
  with `…/artifacts/persisted_artifacts.json`; pipeline snapshots at
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
dispatcher prepends `<MAGICIAN_SKILL_DIR>/bin` to `PATH`. Absolute paths use
`{skill_runtime_root}/bin/<binary>`. See
[capabilities README](../../capabilities/README.md#metabase-pack).

## Canonical References

- **[UNIFIED_AGENTIC_ARCHITECTURE.md](UNIFIED_AGENTIC_ARCHITECTURE.md)** — Agent + Task model, kinds, OEO loop, `delegate_to` allowlist, pack scoping
- **[TRUE_AGENTS.md](TRUE_AGENTS.md)** — Agent runtime, memory, delegation, scheduling, observability
- **[AGENTIC_EXECUTION_DESIGN.md](AGENTIC_EXECUTION_DESIGN.md)** — Core observe-decide-execute loop, dispatch, budgets, loop detection
- **[FLAT_LOOP.md](FLAT_LOOP.md)** — Flat per-action loop, hot/deferred catalog, `tool_search` (replaced inner loop archived at INNER_LOOP_RUNTIME_CONTEXT.md)
- **[IN_CONTEXT_DELEGATION.md](IN_CONTEXT_DELEGATION.md)** — Single-execution owner hops for chat `orchestrate_pipeline`
- **[YIELD_DECISION.md](YIELD_DECISION.md)** — Unified terminal tool
- **[GENERATIVE_AGENT_UI.md](GENERATIVE_AGENT_UI.md)** — GAUI rendering and component lifecycle
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
- p5.5 execution / planning / PlanGraph — Historical pipeline narrative; PlanGraph schema source of truth: `magician/src/magician_v2/strategy/plan.rs`
- AGENTIC_AUTONOMOUS_DESIGN.md — Autonomous execution design
- TRUE_AGENTS_EXECUTION_BOARD.md — Retired True Agents sprint board
- CANVAS_MODE_EXECUTION_BOARD.md — Retired canvas-specialist tracker
- LEGACY_DELEGATED_CHILD_EXECUTION.md — Historical delegated-child-thread model
- PROMPT_SYSTEM_AUDIT_2026-03-02.md — Prompt audit snapshot
