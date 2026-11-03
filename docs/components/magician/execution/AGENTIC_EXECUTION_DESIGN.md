# Agentic Execution Architecture

**Related**:
- [FLAT_LOOP.md](./FLAT_LOOP.md) — hot/deferred catalog, `tool_search`, LLM-less primitive dispatch
- [YIELD_DECISION.md](./YIELD_DECISION.md) — sole LLM-facing terminal tool
- [IN_CONTEXT_DELEGATION.md](./IN_CONTEXT_DELEGATION.md) — single-target chat-pipeline owner swap
- [TASK_STATE_DESIGN.md](./TASK_STATE_DESIGN.md) — durable per-task JSON state
- [LINKED_TASK_ARTIFACTS.md](./LINKED_TASK_ARTIFACTS.md) — `depends_on` / `linked_task_inputs`
- [UNIFIED_AGENTIC_ARCHITECTURE.md](./UNIFIED_AGENTIC_ARCHITECTURE.md) — Task/Agent/delegation model
- [TRUE_AGENTS.md](./TRUE_AGENTS.md) — agent runtime, memory, scheduling
- [agent-definition-reference.md](../agents/agent-definition-reference.md#browser_transports) — `browser_transports` ceiling
- [v2-websocket-events.md](../v2-websocket-events.md) — full event catalog
- Adaptive LLM Routing — follow-on for outcome-aware selection

---

## Overview

An **observe-decide-execute loop** that runs until the model `yield`s, a pause is required, or a budget or scope guard stops it; a planner `PlanGraph`, if any, is rendered into bounded runtime context. No inner-loop LLM for primitive packs and no loop-mode toggle: every run uses the flat per-action catalog (`<pack>__<action>` for primitives, compiled pack names otherwise) with LLM-less dispatch ([FLAT_LOOP.md](./FLAT_LOOP.md)).

The live driver is the **stateless** `run_loop` worker. `MAGICIAN_EXECUTION_DRIVER=inprocess` (exact value) selects the resident rollback arm; unset, blank, `stateless` or unknown (logged at `error`) select Stateless. `driver_inproc::run_iteration` advances one iteration; `driver_worker::advance_once` commits one **phase**; the `StatelessArm` claim-loop (claim → advance → commit → release) makes the worker durable (collapsing it into one `advance_once` would drop per-phase commit).

## Data flow

| Phase | Job |
|-------|-----|
| (before loop) | `execute_agentic_direct_with_outcome` enriches context: durable artifact summary → `environment_knowledge`; linked task artifacts (`depends_on` + execution-pinned `linked_task_inputs`); `merged_agent_tools` / `allowed_action_types` / `denied_tool_params` / `llm_routing` |
| `prepare` | Identity, grants, cancellation, iteration admission |
| `observe` | Recent turns, artifacts, task state, browser evidence (agent-browser; Magicutor supplies CDP when transport=cdp) |
| `decide` | Native catalog (hot + deferred) + decision prompt (`decide_next_action` → `native_decision_via_adapter`) → `Decision` + `ExecutionDecisionEnvelope` |
| `resolve` | New work past the work budget / USD ceiling stops the run; in-flight ops may finish |
| `apply` | Advisory loop-detector pressure → trust/approval/confirmation gates → dispatch Pack (compiled provider or `primitive_dispatch`) → `IterationRecord` |
| `epilogue` | History, stuck warning, iteration-complete event; next iteration or terminal `AgenticOutcome` |

Heap-boxing is a **stability contract**: large runtime-context futures must not sit on Actix worker stacks; `owner_staged_retrieval` (`#[inline(never)]`) keeps the owner-transition hydration frame under 0.5 MB (`tests/agentic_default_stack_contract.rs`). Concurrency admission lives in execution state, provider dispatch queues, per-repo coding shadow locks and tool limits.

---

## Pre-decided decisions (chat pack direct dispatch)

A context with a `PreDecidedPackCall` (`ctx.predecided_call`, from `AgenticContextOverrides.predecided_call`) already knows its one call (a chat turn named one pack leaf and all arguments; [chat mode](../chat-mode.md#chat-pack-calls-pre-decided-vs-model-driven)). `run_loop/phases/predecided_decision.rs` answers DECIDE before the decision rail, a pinned harness or the model.

- **Unsettled**: `Execute` the leaf via `lower_native_tool_call`; anything but exactly one pack call for this leaf fails the run, so it never reaches a control tool.
- **Identity**: trace `synthetic:predecided:<execution_id>:<iteration>` (global iteration, never reused on resume) plus a synthetic assistant turn (operation `agentic_decision_predecided`) with call id `predecided:<chat call id>`; the effect id composes both. `Resolve` treats `synthetic:` ids as loop-synthesized; `AgenticDecisionMade` carries `reasoning: decision:predecided:caller-decided`.
- **`authorize`**: projects the owner's tool policy (`decision_rail_catalog`) as a model turn does. An authorized deferred leaf is promoted like a `tool_search` pick (`promote_predecided_leaf`, intersected with authorized names) and re-projected; an unauthorized one is refused at APPLY and the next DECIDE fails the run; an unresolvable policy fails here. RESOLVE, APPLY (trust, approval rules, allowlist, lineage, effect ids) and OBSERVE are unchanged.
- **Settled**: built with no model or policy projection (a resumed segment, a refused terminal, a held-back fold). Success is `Completed` with the raw output as evidence and `task_deliverable` artifact (empty → `` `<leaf>` completed with no output. ``), so V3 finalization never synthesizes one; failure or APPLY refusal is `Failed`. One proven pre-I/O admission expiry gets one fresh attempt.
- **The fold** (stateless driver only; the in-process driver has no per-phase commits and its body sits at the `the_iteration_body_is_a_driver_and_not_the_loop` size gate): `WorkerHost::fold_after_dispatch` settles the terminal inside the dispatching APPLY, so the run ends (`RunEnded`) in one iteration, and only after `dispatch_apply` succeeds and **`record_batch_outcomes` has made every effect outcome durable**, before the boundary commit (folded task state must never precede the ledger's record of the call). `terminal_to_fold` needs a `NextIteration` dispatch and a `Settled` call (a `RetryAdmission` is unsettled and keeps its fresh attempt); it stands aside (iteration 2 concludes) on cancel, pause, lost scope, any pending steer (in-process queue, host steer buffer, or a durable inbox row read without claiming it), exhausted budget, passed deadline or an owner to yield back to. `fold_settled_terminal` emits `AgenticDecisionMade`, runs RESOLVE and settles via `apply::settle_terminal` (the no-dispatch path of `apply::gate`, without re-emitting the plan-step boundary; trace `synthetic:predecided_settled:<execution_id>:<iteration>`; results pair with the `Execute` turn via `decision::assistant_turn_for_iteration`). An approval pause before dispatch also takes iteration 2. A fold error restores loop state, discards what the host journalled, warns and returns the dispatch's report; it never follows a task-state commit (`yield_back_to_previous_owner` runs are not folded), though an inline-emitted event cannot be withdrawn, and a later lifecycle-exclusion or steer-admission failure keeps the dispatch's durable records.
- **Raw output**: `record_predecided_raw_output` keeps unneutralized text in `ExecutionHistory.predecided_raw_output` up to `PREDECIDED_RAW_OUTPUT_MAX_BYTES` (256 KiB); binary or over-cap results use the projection plus `result_ref`, and the iteration record's bounded `output` is the last resort.
- **Across a pause**: `AgenticPauseState` and the stateless checkpoint carry `predecided_call` and `predecided_settled`, so an approval resume rebuilds an identical action (the waiver applies) and never fires twice; checkpoint raw output ≤ `PREDECIDED_CHECKPOINT_RAW_MAX_BYTES` (32 KiB): projection + `result_ref` when that fits, else text cut at a UTF-8 boundary with a note. Sub-goal and in-context delegation contexts clear both.
- **No hydration**: skips prompt-context hydration and memory-tier/procedure seeding (`seed_direct_execution_context`).

---

## Runtime entry points

| Function | Role |
|----------|------|
| `execute_agentic_direct_with_outcome()` | Entry for direct task, chat-inline, tutor, delegation, VibeDev; resolves the owning agent once |
| `execute_agentically()` | Public loop entry; **heap-boxed** future |
| `execute_agent_cycle()` | Agent-routed cycle (`agent_id` + `goal_id` + `cycle_id`); takes live-agent admission before building the loop future |
| `execute_agentically_resume_with_validation()` / `_resume_exact()` / `_continue()` | Resume; restore `merged_agent_tools` from the scoped pause context |
| `admit_live_agent_loop()` | Live-agent admission before the future is built; rejection does not count as active |

```rust
pub fn execute_agentically<'a>(
    ctx: &'a AgenticContext,
    initial_state: EnvironmentState,
    executors: &'a ActionExecutors,
    cancellation_token: Option<CancellationToken>,
) -> Pin<Box<dyn Future<Output = Result<AgenticOutcome>> + Send + 'a>>
```

Observability needs `execution_id`, `plan_id`, `step_id` (`AgenticContext::has_observability`). Each call resets `ExecutionHistory`, `LoopDetector` and observation context; browser sessions and durable artifacts persist.

| `AgenticContext` field | Role |
|-------|------|
| `goal` / `success_criteria` | Loop objective |
| `max_iterations` | Hard ceiling (default 12000) |
| `work_budget_secs` / `work_budget_consumed_ms` | Optional cumulative active-time budget |
| `max_cost_usd` | Optional USD ceiling (only narrowed by plane attenuation) |
| `max_tokens_per_cycle` | Optional provider-token ceiling |
| `merged_agent_tools` | Authoritative catalog |
| `allowed_action_types` / `denied_capability_names` / `denied_tool_params` | Dispatch ceiling |
| `browser_transports` | Owner transport ceiling |
| `approval_rules` / `trust_level` | Pre-dispatch gates |
| `delegation_targets` / `max_delegation_depth` | Delegation roster (depth default 2) |
| `task_id` / `execution_id` / `root_execution_id` | V3 identity |
| `task_state` | Preloaded durable JSON |
| `depth` / `owner_stack` / `active_owner_agent_id` | Nesting / ownership |
| `chat_inline` | Chat vs autonomous catalog/schema strip |

---

## Native tool calling

Decisions use **provider-native tools** only (`native_decision_via_adapter` errors; no pseudo-tool fallback): `native_catalog.rs` (`NativeExecutionTool` specs), `native_integration.rs` (`AgenticContext` → `CatalogBuildContext`), `native_adapter.rs` (`RouterToolSpec` projection, router call), `native_lowering.rs` (tool calls → `Decision` + `ExecutionDecisionEnvelope`).

Pinned prompts: `agentic_decision` v1.3.12, `agentic_decision_system` v1.0.10 (`magician-core/src/prompts/constants.rs`; files in `data/magician_v2/prompts/`). Provider-native schemas are the invocation-shape authority. `goal_reached` / `cannot_proceed` still lower to `Decision::Yield` for replay but are not in the catalog.

`NativeDecisionOutcome`: `Valid(envelope)`, `ZeroToolCalls` (text-only; adapters may synthesize `Decision::Completed`), `UnknownTool`, `InvalidArguments`. `lower_native_response` folds leading `Execute` calls into one `CandidateBatch`; later non-Execute or unlowerable calls become `deferred_tool_calls` to re-issue; a terminal first call defers all others. `MultipleToolCalls` remains a recoverable variant but is not emitted.

`ExecutionDecisionEnvelope` = `Decision` + `request_hover_discovery`, `request_vision` / `vision_reason` (screenshot, not coordinate fallback), `step_completed` / `step_failed`, `needs_plan_revision`, `task_state_action`, `deferred_tool_calls`, `thinking`, raw payloads. Observation is action-aware (full / quick / DOM-only / skip).

### Catalog assembly (`native_catalog.rs`)

`BUILTIN_LANE_NAMES` is empty; `files` / `http` / `shell` are compiled packs in `direct_capabilities`.

1. **Direct pack capabilities**: the agent's `tools:` allowlist plus `UNIVERSAL_BACKEND_PACKS` (trusted). `trust_level == "untrusted"` gets only `SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED` (`activate_skill`, `deactivate_skill`, `time_math`). Catalog and dispatch ceiling stay in lockstep.
2. **Control tools**: `need_user_input` for chat and autonomous execution (subject to surface policy); `yield`, `read_result`, `export_result` are autonomous-only; `spawn_sub_goal` when depth/budget allow. Decision prompt 1.3.12 asks for essential missing inputs before costly work.
3. **Delegation tools** when targets exist: `delegate_to_agent`, `handover_to_agent`.

Chat mode strips autonomous metadata (`task_state_action`, hover/vision, step signals) and ends with assistant text, not `yield`. One immutable **tool-policy snapshot** (`policy_snapshot.rs`) per decision boundary feeds catalog, dispatch ceiling, deferred index, delegation schema and introspection; a provider schema is never authorization, and whole-tool `denied_tools` apply after control names are assembled, so controls cannot bypass pack policy. Hot/deferred split, `ALWAYS_HOT_TOOL_NAMES`, `tool_search`: [FLAT_LOOP.md](./FLAT_LOOP.md).

### Data by reference and sub-goal hand-off

Large results move between calls without the model re-typing them:

- **Argument references** (`agentic/value_refs.rs`): anywhere in Pack arguments (including JSON strings such as MCP `arguments_json`), `{"$ref": "result_ref_v1_…#/json/pointer"}` becomes the full value at that pointer of an earlier result, and `{"$file": "<path>", "format"?: "text"|"json"}` a file under the task directory (relative to `workdir/`; outside it is refused). Substitution runs at the top of `execute_action_inner`, so gates, approvals and trust policy judge real values; history and evidence keep the compact form. `$ref` has `read_result`'s authority (owner, agent, originating tool grant, authority revision) and is refused wherever `read_result` is denied and in app workflows. Max 32 references / 16 MB per call; a bad reference fails the call.
- **`export_result`** writes the value at `field_path` to `<task>/workdir/exports/<file_name>` (`auto`: strings raw, else pretty JSON; `jsonl` for arrays) and returns the absolute `path`, task-relative `workspace_path` (such as `exports/receipt.json`), its `workspace_root`, bytes, sha256, record count and preview. File tools and governed CLI/MCP file inputs accept either path. `validate_call` on the Decision Engine rail accepts a marker wherever a value is expected ([structured-decision.md](../structured-decision.md)). Served with `read_result` by `read_value_at`; refused in app workflows and to Relentless controllers.
- **Results that age out**: once a record is older than the keep-tail window and the 10 summarized records, `## EXECUTION HISTORY` still lists it by tool, key argument, outcome and result ref, so `read_result` / `export_result` / `$ref` can reach it ([FLAT_LOOP.md](FLAT_LOOP.md#keep-tail-context-window)). `read_file` returns a head excerpt for a large file and takes byte `offset` / `limit` for the rest.
- **Sub-goal hand-off**: `handle_spawn_sub_goal_decision` passes the parent's history summary (≤12 KB, with result refs) into the child's `prior_environment_knowledge` as "PARENT PROGRESS" (same result owner, so refs stay readable); on return the parent gets the child's open items and last observation (≤3 KB).

**Goal-level churn** (`agentic/churn_steer.rs`): `decision_preflight` adds a one-turn steer (never a stop) when, within 15 iterations, the latest call failed and that tool (`capability → inner tool_name` for gateways) failed ≥3 times with ≥2 argument shapes (incl. `is_error`, `remote_error`, `status: error|failed`), or the latest call reads a resource (`result_ref`, `uris`, `id`, `url`, `path`, …) for at least the third time in the window. It names the tool and error and points at untried tools, running saved artifacts through their own system, and `export_result` / `$ref`.

**MCP tool cards** (`execution/mcp_affordances.rs`): governed MCP discovery writes a mechanically derived (never generated) card per pack (one line per remote tool: required/optional args, first description sentence) to `scopes/<p>/<w>/runtime/mcp_affordances/<pack>.json` (on fingerprint change), appended to the pack guide under "LOADED TOOL GUIDES" when the pack loads.

**Task working directory** `<task>/workdir`: `shell` starts there (`TASK_SHELL_ROOT`, from runtime identity, never arguments); `files` / native `File` (`native_executors::execute_file_action`) admit it (`task_file_sandbox`) and anchor relative paths there (`task_relative_file_action`), plus owner-approved session roots; `write_file` / `edit_file` skip diff approval only inside it (`path_in_task_workdir`, on the canonical deepest existing ancestor, so `..` and symlink escapes are refused); workspace-mode CLI skills (marimo, office-excel, …) start there (`PrimitiveExecCtx::task_work_root`, only when it is a directory inside the scope) unless a Quality run's work root applies. Evidence capture (`capture_pack_action_artifacts_projection`) records files named by `file`, `path`, `output`, `output_file`, `output_path`, `out`, `destination` or `-o` / `--output` that changed in the last 15 minutes in task or scope workdirs.

Governed CLI and MCP file arguments share that relative-path root (`bind_runtime_workspace_path`); without a task they use scope `workdirs`. Absolute paths may select files under the current task or scope workdirs, with the same traversal/symlink checks; a Quality root remains exclusive. Relative paths never search other roots, and a missing task root fails closed. Failures name the chosen root. Declared MCP output pointers may publish current-task files as well as their reviewed scope output directories; other task and host files remain excluded.

### Lowering (`native_lowering.rs`)

Dedicated arms: `yield`, `need_user_input`, `spawn_sub_goal`, `delegate_to_agent`, `handover_to_agent`, and `goal_reached` / `cannot_proceed`. **Everything else** (files, http, shell, search, fetch, `tool_search`, `browser`, `duckdb`, Yutori bare names → `browser__<action>`) goes `lower_pack_execute` → `ExecutableAction::Pack`; `ExecutableAction::File` / `Http` / `Bash` are compiled-provider internals only. Metadata sidecar fields are stripped before capability code.

---

## Tool surface

`merged_agent_tools: Vec<ToolInfo>` (`MagicianV2Orchestrator::resolve_merged_agent_tools[_for_scope]`) is authoritative. **Direct ownership wins** over a delegate listing the same tool; among delegates the first lexicographic agent id owns it. Delegate-owned tools carry `providing_agent_id` (use `delegate_to_agent`). Resume restores the list from the pause context, never `0 tools`.

### Universal substrate (`UNIVERSAL_BACKEND_PACKS`)

Trusted agents only.

| Group | Packs |
|-------|-------|
| Memory | `save_preference`, `update_memory_tier`, `search_memory`, `forget_memory`, `list_memory_tiers` |
| Introspection | `list_tasks`, `list_agents`, `inspect_agent`, `get_agent_details`, `find_agents_for_capability`, `list_artifacts`, `list_scheduled_tasks`, `list_episodes`, `list_proposals`, `get_active_executions`, `get_execution_history`, `read_program_state`, `read_trace`, `system_status`, `time_math` |
| Task ops | `stop_task`, `update_task`, `delete_task`, `refine_task`, `propose_relentless` |
| Surfaces | `create_dashboard`, `unpublish_dashboard` |
| Identity / skills | `switch_personality`, `activate_skill`, `deactivate_skill` |
| Meta / files | `tool_search`, `edit_file`, `glob`, `grep`, `files`, `read_file`, `write_file` |
| HTTP / shell | `http`, `shell` |
| Acquisition | `content_search`, `content_read`, `web_fetch`, `web_search`, `web_answer` |

`task_state` only when `ctx.task_id` is set. Admin packs (`create_agent`, `treasurer`, `notify_owner`, `create_proposal`, …) are not on this rail. **Progressive disclosure**: the focused capability gets its full pack spec in DECIDE, others compact summaries; unfocused browser defaults live in `skillshub/browser/SKILL.md`.

---

## Decisions and outcomes

| `Decision` (`decision.rs`) | Source |
|---------|--------|
| `Execute { candidates, thinking }` | Work tool call (always a `CandidateBatch`) |
| `Yield { payload }` | LLM terminal; `dispose_yield` → Completed / PartialSuccess / Failed / RetryTransient |
| `NeedUserInput { … }` | Pause primitive (not terminal) |
| `SpawnSubGoal { goal, unblocks, budget_iterations }` | Inline nested loop |
| `DelegateToAgent { targets }` | Isolated children (or in-context single-target) |
| `HandoverToAgent { … }` | Same-execution owner swap |
| `Completed` / `Failed` | Synthetic fallbacks only (text-without-tool, malformed args) |

`AgenticOutcome` (`types.rs`): `Success`, `Failed`, `MaxIterationsReached` and `BudgetExhausted` (pausable when `pause_state` is `Some`), `LoopDetected` (stored outcomes only), `WaitingForUser`, `WaitingForConfirmation`, `PausedByUser`, `WaitingForChildren`, `CannotProceed`, `Sleeping` (persists state, schedules a `WakeUpQueue` timer).

`UserInputType`: `Text`, `Password`, `Choice`, `MultiChoice`, `Confirmation`, `ExternalAction`, `FilePath`, `Guidance`, `ToolAuthorization`, `SandboxOverride`.

---

## Dispatch

`CapabilityRegistry` holds `CapabilityProvider`s; lowering always emits `ExecutableAction::Pack`.

| Pack kind | Provider / dispatcher |
|-----------|------------------------|
| Compiled filesystem / HTTP / shell | `FileCapabilityProvider` (`files`, `read_file`, `write_file`), `HttpCapabilityProvider`, `ShellCapabilityProvider` (`compiled_providers.rs`); run `File` / `Http` / `Bash` internally |
| Compiled search | `SearchCapabilityProvider` |
| Compiled task state | `TaskStateProvider` (when `ctx.task_id` is set) |
| YAML `composite` / `command` | `PackCapabilityProvider` (`pack_provider.rs`) |
| YAML `primitive` | `primitive_dispatch` (`dispatch.rs`): browser → pinned `agent-browser` CLI; DuckDB etc. keep Rust providers; CLI packs use `ScopedDeterministicCapabilityInvoker` |

`ImplementationType` (`capability.rs`): `Composite { steps }`, `Compiled { provider_name }` (metadata only), `Primitive { … }`, `Command`. `PackCapabilityProvider` uses the YAML type and re-resolves params (required, aliases, defaults) even for raw DECIDE actions. Command packs drop omitted optional params, fail missing required ones before launch, map aliases to the canonical field (stable `arg_mappings` and env templates), and recover nested `body="{...}"` JSON for declared fields.

### Browser

A primitive pack (`BROWSER_PACK_NAME = "browser"`): `browser__<action>` leaves, driven by `dispatch_browser_primitive` through the pinned agent-browser CLI. `connection_mode` is a **request**, not authorization: retrieval-handoff exact mode → call `connection_mode` → `MAGICIAN_AGENT_BROWSER_MODE` → default `cdp`, held to the **owner agent's** `browser_transports` ceiling (empty = `cdp` / `headed` / `headless`) by `BrowserTransportCeiling::resolve`. Out-of-ceiling transports are **refused**, never substituted; the ceiling is replaced, not merged, at each owner transition. Failures return to the loop with no hidden mode fallback or coordinate-click retry (`AgenticClickFallbackUsed` is for old clients).

---

## Budgets

| Dimension | Where | Default |
|-----------|-------|---------|
| Iterations | `AgenticContext.max_iterations` (`with_max_iterations`) | 12000 |
| Wall-clock work | `work_budget_secs` | `None`; active time, cumulative across pause/resume |
| Wall-clock ceiling | `MAGICIAN_AGENTIC_MAX_DURATION_SECS`; `agentic_max_duration()` | 86400 s (24 h) when unset for delegated, sealed and app runs and for chat/plane turns; `0` disables; a watchdog cancels the run at the deadline. An ordinary task-backed root run has **no** wall clock unless the env var is set (`task_run_max_duration()`): it is bound by liveness, the spend envelope and its valves. Background shell jobs default to 72 h (`shell_job_max_duration()`) |
| USD cost, native run | `resource_authority.model_spend.run_magician_usd` (live config), overridden by `MAGICIAN_AGENTIC_MAX_COST_USD`; `agentic_max_cost_usd()` | `$100` per-run valve (`DEFAULT_AGENTIC_MAX_COST_USD`); `"unlimited"` (or env `0` / non-positive) disables; checked before new work |
| Spend envelope | limits in `resource_authority.model_spend` (`window_usd`, `window`, `lifetime_usd`), behaviour in `execution.budgets.spend_envelope` (`enabled`, `reset_offset_minutes`, `warn_at_pct`, `unpriced`); `spend_envelope.rs`, posted to the ledger by `spend_envelope_ledger.rs` every 15 min; grants in `system/spend_envelope_grants.json` | On at `$250` per UTC day, counted at the router for all LLM work. When spent, autonomous work (autonomous task, scheduled, ambient, memory, comms assist) parks (the decision retries with a wait capped at 10 min, no counter moves) until the window resets or an owner grant; chat, interactive tasks, evaluation and system calls are counted but never blocked. One `spend_envelope:exhausted` attention item; an `unpriced` model is estimated at the provider's highest known rate (or blocked) |
| USD cost, harness run | `resource_authority.model_spend.run_harness_usd`; `harness_max_cost_usd()` | `$100`; `"unlimited"` disables; spend = CLI-reported `cost_usd` per settled turn (none reported charges nothing) |
| Tokens | `max_tokens_per_cycle` (native only) | `None`; no pause while the spend envelope governs (the meter counts cached tokens, so it tracks context resend, not money); harness runs get **no token meter** (they resend full context). `MAGICIAN_STRICT_TOKEN_USAGE` (off) fail-closes on missing usage |
| Repeated actions | `max_repeated_actions` | 3 |

A **harness run** is one whose engine (launch pin, else Settings) resolves to a launchable harness (`resolve_turn_engine` → `TurnEngine::Harness`); `magician` is native (`run_budget.rs`).

**Owner grants** (`run_budget.rs`): `budget_grants.json` (execution dir, beside `engine_pin.json`, outside the authority-hashed pause record) is an append-only list of `{granted_at, add_cost_usd?, add_tokens?, add_iterations?, add_duration_secs?, unlimited?, source}`; publication uses the shared durable writer (file and parent-dir sync); append refuses an unreadable or malformed file rather than replace its history. Ceilings are derived, never stored: cost cap = base + Σ `add_cost_usd` (none if any `unlimited`), lowered by a plane-delegated `max_cost_usd`; token limit = `max_tokens_per_cycle` + Σ `add_tokens` (none for harness or `unlimited`). The loop polls grants at each iteration/phase-entry check (one `stat`, re-read only on change) and moves its own token meter (`set_owned_execution_token_limit`; not a shared inline one); `add_iterations` raises the next resumed segment's ceiling; a running segment's ceiling is fixed.

**Extend and continue**: iteration, cost, token and ordinary run-time exhaustion ask through the canonical confirmation card; approval continues the same execution with retained progress and cumulative usage. Iteration approval records a fresh original-sized `add_iterations` allowance (existing unused grants are consumed first), applied once across retries and cold resumes. Other answers grant no new allowance; restoring a checkpoint does not implicitly raise its iteration ceiling. Every resume runs `apply_budget_grants_on_resume`, which mints a grant only for an exhausted, uncovered dimension on an approved budget continuation (never for an unrelated answer or placement recovery): cost gets a `source: "continue"` allowance of the original cap beyond recorded spend (incl. overage, without clearing the accumulator); active time gets one fresh configured allowance (default 24 hours) as `add_duration_secs` (explicit duration grants apply once, `duration_grants_applied_secs`); native tokens get a base-sized grant. Wall-clock expiry is the same resumable pause with zero remainder, judged against the admitted segment's deadline so a later config change cannot relabel it; resident, worker-entry and final pre-dispatch checks share the reducer. Ordinary grants never widen delegated parent or active-work ceilings (parent ceilings are intersected after grants), app contracts or controller-owned attempts (incl. Relentless, which keep frozen allowance and spend; the API refuses those roots). Accepted redirects stay behind the resumable segment fence; cancellation and manual-pause fences never become continuations. Content-free `budget_usage.json` holds cumulative counters (incl. the final pause charge and prior segment iterations), published at pause settlement via shared durable publication, for cold reads; it grants no authority and cannot resurrect a reclaimed scope. Owner API `GET/PUT /budgets/defaults`, `GET/POST /tasks/{task_id}/budget`: [magician-api](../../magician-api/README.md#run-budgets); the composer offers `+24 hours`.

**Provider failure**: when a decision's provider outage or transient retry budget runs out, an ordinary task-backed root run pauses on the same card with **Retry** / **Cancel** instead of failing (no fallback model); Retry grants nothing and resets the provider retry counters, Cancel ends the run as cancelled. Retried provider calls are refunded like spend-envelope parks. Delegated, sealed and app runs still fail. Contract: [termination](../agentic-loop-termination.md#provider-outages-and-long-runs).

Foreign harness slices advance through a host-generated durable checkpoint (native session/home and partial-result note ride the phase store), keep cumulative usage, obey the enclosing deadline and never invoke owner budget continuation ([Plane](../plane.md)). Harness Work uses typed `yield` / `need_user_input` under the outer loop's evidence and owner-input rules (plain harness text has no completion authority); its owner-input pauses carry native session/home metadata and replies use the built-in sensitive-answer formatter.

There is no `max_llm_calls` field (`BudgetDimension::LlmCalls` stays on the enum). At a limit an in-flight op may finish, nothing new starts, and findings go through `goal_achieved_partial` collation. Delegation `timeout_secs` is a **soft work budget**, not a cancel timer: tier `normal` / `deep` / `thorough` = 900 / 2700 / 5400 s (`actions.rs::resolve_delegation_work_budget_secs`); an explicit value only extends its tier (named depth, else normal) and `0` is rejected; neither present → none; never clamped by the source's `delegation_timeout_secs`. Invalid budget/depth fails the all-or-none preflight. Strategy exploration budgets (`strategy/types.rs`) bound planning only: 100 LLM calls, 2 h, 80k tokens, confidence 0.7.

---

## Loop detector

`loop_detector.rs` fingerprints the last 20 records (`check_before_action`) and only **appends advisory progress-pressure**; hard budgets are the stop.

| Kind | Rule |
|------|------|
| `StateLoop` | similarity > 0.95 (`STATE_SIMILARITY_THRESHOLD`), seen ≥ 3 |
| `ActionCycle` | repeating A→B→A→B |
| `NoProgress` | state unchanged after actions, similarity > 0.90, ≥ 5 |

Browser fingerprint weights (total 12.5): URL 3.0, DOM hash 2.0, content hash 2.0 (skipped if empty), scroll bucket 2.0 (same 5% bucket full, adjacent partial), interactive count 1.0 (±2), modal / error 1.0 each, title hash 0.5.

---

## Step-stuck warning and adversarial reviewer

### `AgenticStepStuckWarning`

Observability only; fires at `consecutive_no_action_iterations >= 3` (`STUCK_WARNING_THRESHOLD`, `run_loop/phases/epilogue.rs`; `[STUCK-WARNING]` log) until a successful tool call. `is_preflight` (default false) marks historical pre-loop probes; bootstrap and resume never run catalog-wide probes (`executor.rs`), and `preflight.rs` serves setup surfaces only.

### Adversarial progress reviewer

A task-backed **read-only streak** at `NO_PROGRESS_AUTO_YIELD_THRESHOLD` (10, `decision.rs::detect_stuck_kind`) runs `run_adversarial_reviewer` (operation `progress_review`): `continue` / `converge` / `redirect` inject a `[progress-review]` steer; `escalate` → `try_synthesize_stuck_auto_yield`. It re-fires at 13/16/19 and hard-concludes at 20. Repeat-churn (`NO_PROGRESS_REPEAT_AUTO_YIELD_THRESHOLD = 3`), non-task-backed runs and 5 trailing same-class failures (`StuckKind::RepeatFailure`) hard-conclude; that `Yield` skips resolve's structural deny ([loop termination](../agentic-loop-termination.md#repeated-same-class-failures)). A prompt nudge (`STUCK_SIGNAL_THRESHOLD = 3`, `STUCK_AUTO_YIELD_THRESHOLD = 5`) comes first. Telemetry target `agentic.reviewer`.

---

## Pause and resume

`NeedUserInput` → `WaitingForUser` (workflow PAUSED) → API reply → `FullPauseStore.resume(user_input)`. `PausedByUser` needs no input. `WaitingForConfirmation` pauses destructive actions (`action_summary`, `reason`, `action_type`, `action_json`, optional secret-approval challenge); `action_json` is the token from `stable_confirmation_action_json`, consumed once on exact token-and-owner match. **Approved-call replay** (`run_loop/phases/approved_replay.rs`): the pause also records the model's tool call behind the gated action (`AgenticPauseState::confirmation_replay_call`, kept only when that call lowers to exactly the gated action), an approving resume attaches it to the approval (`ApprovedConfirmationAction::replay_call_json`, also for alignment holds), and the first DECIDE returns that call instead of asking the model, under a synthetic turn (`synthetic:approved_replay:…`, call id `<original>-approved`) after the same lowering and owner policy projection a model turn makes; RESOLVE, APPLY (the waiver included) and OBSERVE are unchanged and later iterations are model-decided. A replay is one-shot (`LoopProtectiveState::approved_replays_attempted`, ≤ 32): an approval APPLY does not consume falls back to the model, and a call that no longer lowers to the approved action is never replayed. Producers fail closed if the action cannot be serialized (never stored with an empty token), and an empty token never matches, even in a legacy pause. `AgenticPauseState` carries iteration, environment, history summary, tool scope, budgets, owner stack and routing overlays.

### Shared direct-agent override builder

Chat-inline delegation, delegated children and task-backed direct runs (initial and resume) share one path: resolve the owner once, derive `merged_agent_tools` / `allowed_action_types` / `denied_tool_params`, keep per-operation `llm_routing` overrides, call `execute_agentic_direct_with_outcome()`; task-only controls (output mode, pipeline stages) layer on top.

A caller route is written to `executions/<execution_id>/llm_routing_overrides.json`, copied to each delegated child before launch and reloaded on every rebinding and resume; hot paths read it with known scope (execution-id discovery only for legacy scope-less callers); it never mutates the task manifest or agent definition. It stays separate from refreshable owner defaults; `owner + execution` routing is recomputed at each owner boundary, decision/critic/compaction adapters are built from the merge at call time, and pause state keeps both.

---

## Spawn sub-goal

`spawn_sub_goal` runs **inline** (no new Task), needs non-empty `goal` and `unblocks`. Budget default 75 (`DEFAULT_SUB_GOAL_BUDGET`), clamped to 1..=400, then to `min(budget, parent remaining − 1)`; zero rejects, and the tool is withheld when unfundable. `ctx.depth >= max_delegation_depth` (default 2, `constraints.coordination.max_delegation_depth`) → `DepthLimitReached`; high trigram overlap with the parent → `SubGoalParaphrasesParent` (failed iteration). Approval rules are inherited (no privilege escalation), parent state cleared, depth incremented; the nested future is heap-boxed; artifacts roll up to parent history.

---

## Delegation

Runtime tools: `delegate_to_agent`, `handover_to_agent` (no `create_task` on this loop).

- **Chat-inline `delegate_to_agent`** has no GuidedSearch / AtomicComposition or mode switches and returns after starting the child; progress arrives on the `chat` progress channel. Inline delegates are task-backed and inherit `(principal, workspace, ui_thread_id)`.
- **`handover_to_agent`** (same-execution ownership transfer) needs `preserve_live_execution_context=true` **and** a real continuity anchor (active browser/app/session); within a specialist chain, yield back or delegate.
- **In-context single-target** `orchestrate_pipeline` hops: [IN_CONTEXT_DELEGATION.md](./IN_CONTEXT_DELEGATION.md); others spawn children and the parent enters `WaitingForChildren`.
- **Typed named-delegation launch**: a V3 execute request may name one `delegate_to_agent`, skipping only root-model routing (policy, cycle/depth, idempotency, admission and isolation still apply); admission locks are keyed by parent execution.
- **Idempotent reconciliation**: targets are canonicalized and fingerprinted with the parent; duplicates collapse, retries reconcile existing children, failed/cancelled attempts may get a new revision; a replay of only completed children never enters `WaitingForChildren`.
- **Post-delegation**: the V3 orchestrator owns only `WaitingChildren → Runnable`; Artifact V2 owns the parent, prompted with child deliverables to conclude, delegate a named gap, or yield. Continuation is a lazy execution-runtime job (never nested under the unwinding child executor), capped at four reconciliation decisions. A first Markdown/plain-text run projects task-user output through the grounding boundary; there is no separate task-agent output.
- **Exactly-once terminal synthesis**: one primary-output revision per execution; completion, retry and startup recovery share one claim set, children one single-flight owner; a mid-request crash may retry but commits once.
- **Tree-wide cost identity**: every task-backed provider call (root, children, resumes, synthesis, review, reflection) carries one `root_execution_id`; the scoped `llm_calls` ledger is the money source.

---

## Task state, artifacts, and research evidence

`task_state` is a compiled capability over `DurableArtifactStore` (namespace `"task_state"`), preloaded into `AgenticContext.task_state`, rendered as `{task_state_section}`, carried on `AgenticPauseState`. `task_state_action` is optional **inline metadata** on the tool call (default `none`, strictly validated; create/patch/close contract in the decision prompt). `try_register_durable_file_action()` stamps `source_task_id` on durable-store writes (atomic rename) and records `durable_artifacts_written` ([LINKED_TASK_ARTIFACTS.md](./LINKED_TASK_ARTIFACTS.md)). Taskplan spills are a **storage optimization**: critical outputs stay in recent history; spills are promoted to the scoped store, never re-spilled on read, and share one loop-detection family.

`content_search` / `content_read` take a scalar or a `requests` array. Vector `content_search` keeps each admitted candidate's URL and snippet as discovery-only (`claim_eligible: false`). Read evidence carries requested/final URL, redirect state, title, excerpt; `fetch_status` is transport completion only.

Synthesis preserves evidence order (a later direct success supersedes an earlier failure; a failure stays only if it leaves a gap) and adds no unsupported claims. **Research precision review** checks a completed/partial draft against successful `content_read` records marked `opened_page` and `claim_eligible`: faithful → earlier attempts closed as superseded; unsupported → one correction turn, then fail closed; critic outage → deterministic terminal gates.

---

## Decision metadata and rationale

One optional open `decision_metadata` object maps into the envelope (conflicting flat/nested values fail closed; chat strips it). `thinking` ≤ 240 chars (`MAX_DECISION_RATIONALE_CHARS`), falling back to a tool-name label; folded turns keep per-candidate rationales. Native responses are captured **before** lowering, and `ExecutionHistory.format_for_llm(...)` renders a bounded recent assistant-turn section; hidden reasoning is never stored. `## CURRENT STATE` (`EnvironmentState::format_for_llm_with_replayed_result(iterations_in_messages > 0)`) references a result already in replayed messages ("delivered whole as the tool result above (N bytes)") instead of a truncated copy, for shell/pack states; browser, filesystem and http states render unchanged.

---

## Provider context reuse

| Providers | Strategy |
|-----------|----------|
| **OpenAI Responses**, opted-in **Gemini Interactions** | Bootstrap once, then deltas via provider checkpoint id (tools/system/generation controls resent where Gemini requires); re-bootstrap after six turns or on provider/model/endpoint/API-mode, image, compaction, resume or anchoring change; never mixes a checkpoint with replayed history |
| **Anthropic, OpenAI Chat, Gemini `generateContent`, MiniMax, DeepSeek, OpenRouter** | Full bounded transcript, prefix caching |
| **Ollama, Yutori, unknown/custom** | Bounded replay, no cache contract |

The local prompt fingerprint is never sent upstream.

---

## Realtime events

| Event | Trigger | Notable fields |
|-------|---------|----------------|
| `AgenticExecutionStarted` | Loop begins | `goal`, `success_criteria`, `max_iterations`, optional `hint_action` / `agent_id` |
| `AgenticIterationStarted` | Each iteration | `iteration` (1-indexed), `environment_type` |
| `AgenticIterationCompleted` | End of iteration | `duration_ms`, `outcome` (`action_executed` / `decision_rejected` / `loop_continue` / `terminal` / `paused` / `cancelled`) |
| `AgenticDecisionMade` | LLM decides | `decision_type`, `action_summary`, `thinking`, `tool_name`, `candidates_count`, `raw_decision` |
| `AgenticActionExecuted` | Action completes | `action_type`, `target`, `success`, `latency_ms`, `error` (capped at 2,000 chars, ending in `…` when cut) |
| `AgenticStepStuckWarning` | No-success streak ≥ 3 | `consecutive_count`, `recent_actions` (≤3), `is_preflight` |
| `AgenticPageUnderstanding` | Multimodal page understanding | `observation_id`, `page_stage`, `element_count`, `has_screenshot` |
| `AgenticExecutionCompleted` | Loop ends | `outcome` (`success`, `failed`, `max_iterations_reached`, `loop_detected`, `waiting_for_user`, `waiting_for_confirmation`, `budget_exhausted`, `cannot_proceed`), `iterations_used`, `artifacts`, `duration_ms`, `summary` |

HITL payloads ride `HitlRequested { source: "agentic" }`, not the slim `AgenticWaitingFor*` markers.

---

## LLM routing

Static order: `llm.router.operation_mapping` profile → optional `when_has_images` alternatives → authorized request/agent overrides → pinned locks for fail-closed local-only operations → fallback/retry → chat-only fast/thinking via `request_thinking_mode`. No live complexity classifier.

---

## Scope-loss guards

A task-backed execution lives at `{scope}/tasks/{task}/executions/{exec}/execution.json` (`resolve_runtime_execution_dir`); writes to a vanished scope fail with `missing_v3_execution_scope`. Scopes can vanish mid-run (`delete_execution` is `remove_dir_all`; a terminal → `Executing` transition can revive a reclaimed id).

1. **Resumer self-terminate** (`trigger_goal_awaitable`): `Completed(Ok)` plus a permanent status error (`not found` / `missing_v3_execution_scope`) fails the task; transient errors retry.
2. **Loop scope-liveness** (`execute_agentically`): from iteration 2, resolver `Ok(None)` finalizes Failed (`[AGENTIC-SCOPE-LOST]`); transient errors never abort.
3. **Cancel-before-delete**: `delete_execution` cancels first; loop-entry `get_execution` fails closed on late revivals.

---

## Delegation continuation: active-root reclaim

A delegated child **shares its parent's `task_id`**. On child terminal, `reconcile_waiting_children_and_continue` → `continue_task_backed_parent_after_child_completion` → `activate_execution(scope, task_id, parent_execution_id)` re-claims the root: an atomic check-and-swap under the process-global `active_root_swap_gate` (`artifact_v2/service.rs`) that reclaims only a **definitively terminal** holder (`completed` / `failed` / `cancelled`) or `ExecutionNotFound`; live/paused/waiting holders block and transient/corrupt reads fail **closed**. The reducer writes `active_root_execution_id` under another lock, so a lost update can strand a root but **cannot produce two live roots** (only roots write the field; reclaim is terminal-only).

---

## Key files

| Component | Path |
|-----------|------|
| Executor / cycle / spawn / durable hook | `magician/src/magician_v2/execution/agentic/executor.rs` |
| Stateless loop phases + drivers | `…/agentic/run_loop/` |
| Decision + stuck/reviewer | `…/agentic/decision.rs` |
| Types, outcomes, pause, history | `…/agentic/types.rs` |
| Loop detector | `…/agentic/loop_detector.rs` |
| Native catalog / adapter / lowering / integration | `…/agentic/native_*.rs` |
| Yield disposition | `…/agentic/yield_decision.rs` |
| Tool-policy snapshot | `…/agentic/policy_snapshot.rs` |
| Preflight diagnostics (not bootstrap) | `…/agentic/preflight.rs` |
| Delegation spawn / ownership hops | `…/agentic/delegation_dispatch.rs`, `…/agentic/ownership_runtime.rs` |
| Action enum | `magician/src/magician_v2/execution/actions.rs` |
| YAML packs / compiled providers | `…/execution/pack_provider.rs`, `…/execution/compiled_providers.rs` |
| Primitive dispatch / browser ceiling | `…/execution/primitive_dispatch/dispatch.rs` |
| Task state | `…/execution/task_state_provider.rs` |
| Merged tools | `…/orchestrator/v2_orchestrator.rs` |
| Events | `…/realtime_events.rs` |
| Active-root gate | `…/artifact_v2/service.rs` |
| Strategy budgets | `…/strategy/types.rs` |
| Prompt pins / decision prompt | `magician-core/src/prompts/constants.rs`, `data/magician_v2/prompts/agentic_decision_v1.3.12.json` |
| Browser skill | `skillshub/browser/SKILL.md` |

Merkle helpers (`execution/merkle.rs` / `types.rs`) serve only historical `PageState` records; Set-of-Mark observation is retired.

---

## Multi-Step Orchestration Patterns

All paths converge on `execute_agentically()`: a stored `PlanGraph` is lowered for validation and rendered into runtime context (goal, success criteria, initial URL); there is no pipeline step executor. Callers build `AgenticContext::new(goal, success_criteria).with_max_iterations(n).with_observability(execution_id, plan_id, step_id)` and map outcomes: `Success` → complete; `WaitingForUser`/`WaitingForConfirmation` → collect input, resume; `WaitingForChildren` → reconcile parent; `Sleeping` → schedule wake; `Failed`/`BudgetExhausted`/scope lost → fail (budget may be resumable). Split a **taskplan** step when expecting many UI actions, branching, intermediate verification or major page-context changes (navigation, login, modal flow).

### What a scoped run needs before the default driver will start it

The stateless arm refuses rather than falling back to the resident loop. It needs **execution id, principal and workspace** (`StatelessArm::new` names the missing one), **an `OwnershipRuntime`** on the executors (trait defaults refuse rather than guess), and **a real Artifact task and execution** whose owner is named on the context and, on resume, in the pause snapshot (`AgenticPauseState::with_agent_routing`). A **task-backed** run (`task_id` set) cannot complete-yield without material evidence. Every dispatch needs an effect id (LLM call id + tool-call id), else the loop refuses to fire; scripted fixtures need a trace receipt and distinct tool-call ids.

On the stateless arm, **steer** goes to the durable inbox (`run_loop::steer_inbox`, claimed against the sealed control generation, runtime row `Executing`), not the process-local `steer_queue`; **manual resume** uses `claim_stateless_manual_resume` under a sealed tree transaction (resident: `take_manual_pause_for_execution_with_recovery`). Reading the resident control reports a stateless run as inactive.

Steer applies at the next decision boundary: failed decisions release their sealed claim for bounded retry; new guidance supersedes automatic approval replay and historical churn stops, and keeps tools available even when the previous micro-goals were complete. Both drivers refuse steering a non-executing row.

Tests: `magician/tests/driver_phase_differential.rs` (one script, both arms); `executor.rs`'s `scoped_loop_run` for in-crate tests; stack-fit tests use tokio's 2 MiB worker stack, as production does.

Verification runner v3 lets macOS dyld open the root directory as its `openat` anchor (per `dyld-support.sb`); descendant contents, outside writes, private files and network stay denied.
