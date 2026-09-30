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

Agentic execution is an **observe-decide-execute loop**. A planner `PlanGraph` (when used) is **rendered into bounded runtime context**; the executor loops until the model `yield`s, a pause is required, or a budget/scope guard stops the run.

No inner-loop LLM for primitive packs and no `ExecutionLoopMode` toggle: every run uses the flat per-action catalog (`<pack>__<action>` for primitives, compiled pack names otherwise) with LLM-less dispatch. See [FLAT_LOOP.md](./FLAT_LOOP.md).

```
Observe context ──▶ Decide (native tools) ──▶ Execute (dispatcher)
      ▲                                               │
      └───────────────────────────────────────────────┘
Exit: yield / pause / children / budget / scope lost
```

The live driver is the **stateless** `run_loop` worker. `MAGICIAN_EXECUTION_DRIVER=inprocess` (exact value) selects the resident rollback arm; unset, blank, `stateless`, or unknown (logged at `error`) select Stateless. Both drivers run the same six phases:

| Phase | Job |
|-------|-----|
| `prepare` | Identity, grants, cancellation, iteration admission |
| `observe` | Grounding / evidence into runtime context |
| `decide` | Native tool-call (`decide_next_action` → `native_decision_via_adapter`) |
| `resolve` | Budget/work-boundary checks before new work starts |
| `apply` | Dispatch the lowered `Decision` |
| `epilogue` | History, stuck warning, loop pressure, iteration-complete event |

`driver_inproc::run_iteration` advances one iteration; `driver_worker::advance_once` advances and commits one **phase**. The `StatelessArm` claim-loop (claim → advance → commit → release) is what makes the worker durable — collapsing it into one `advance_once` would drop per-phase commit.

Heap-boxing is a **stability contract**: large runtime-context futures must not sit on Actix worker stacks. Concurrency admission stays with execution state, provider dispatch queues, per-repo coding shadow locks, and tool-specific limits.

---

## Data flow

```
0. CONTEXT ENRICHMENT (execute_agentic_direct_with_outcome, before the loop)
   • durable artifact summary → environment_knowledge
   • linked task artifacts (depends_on + execution-pinned linked_task_inputs)
   • merged_agent_tools / allowed_action_types / denied_tool_params / llm_routing
1. OBSERVE   recent turns, artifacts, task state, browser evidence (agent-browser
             snapshots/screenshots/eval; Magicutor supplies CDP connectivity when transport=cdp)
2. DECIDE    native catalog (hot + deferred) + decision prompt → Decision
             (Execute / Yield / NeedUserInput / Spawn / Delegate / Handover)
             + ExecutionDecisionEnvelope side-channel
3. RESOLVE   decision starts new work and work budget / USD ceiling crossed → stop;
             in-flight ops may finish, no new model turn or delegation
4. APPLY     loop-detector pressure (advisory) → trust/approval/confirmation gates →
             dispatch Pack (compiled provider or primitive_dispatch) → IterationRecord
5. EPILOGUE  next iteration or terminal AgenticOutcome
```

---

## Runtime entry points

| Function | Role |
|----------|------|
| `execute_agentic_direct_with_outcome()` | Orchestrator entry for direct task, chat-inline, tutor, delegation, and VibeDev runs. Resolves the owning agent once and hands one override bundle to the loop. |
| `execute_agentically()` | Public loop entry; returns a **heap-boxed** future. |
| `execute_agent_cycle()` | Agent-routed cycle (`agent_id` + `goal_id` + `cycle_id`); default `max_iterations` 4000. |
| `execute_agentically_resume_with_validation()` / `_resume_exact()` / `_continue()` | Resume paths; restore `merged_agent_tools` from the scoped pause context. |
| `admit_live_agent_loop()` | Hard live-agent admission before the cycle future is built; rejection does not increment the active count. |

```rust
pub fn execute_agentically<'a>(
    ctx: &'a AgenticContext,
    initial_state: EnvironmentState,
    executors: &'a ActionExecutors,
    cancellation_token: Option<CancellationToken>,
) -> Pin<Box<dyn Future<Output = Result<AgenticOutcome>> + Send + 'a>>
```

Observability events require `execution_id`, `plan_id`, and `step_id` (`AgenticContext::has_observability`). Each call resets in-memory `ExecutionHistory`, `LoopDetector`, and observation context; browser sessions and durable artifacts persist.

`AgenticContext` (selected fields):

| Field | Role |
|-------|------|
| `goal` / `success_criteria` | Loop objective |
| `max_iterations` | Hard ceiling (default 4000) |
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

The decision transport is **provider-native tools** only; there is no pseudo-tool fallback, and `native_decision_via_adapter` errors if the native path fails.

```
native_catalog.rs       build NativeExecutionTool specs
native_integration.rs   AgenticContext → CatalogBuildContext; native_decision_via_adapter
native_adapter.rs       project to RouterToolSpec, call router
native_lowering.rs      tool calls → Decision + ExecutionDecisionEnvelope
executor / run_loop     observe → decide → execute
```

Pinned prompts: `agentic_decision` v1.3.7 and `agentic_decision_system` v1.0.7 (`magician-core/src/prompts/constants.rs`; files under `data/magician_v2/prompts/`). Provider-native schemas are the invocation-shape authority. `goal_reached` / `cannot_proceed` still lower to `Decision::Yield` for replay but are not in the catalog.

`NativeDecisionOutcome`: `Valid(envelope)`, `ZeroToolCalls` (text-only; adapters may synthesize `Decision::Completed`), `UnknownTool`, `InvalidArguments`. `lower_native_response` folds consecutive leading `Execute` calls into one `CandidateBatch`; later non-Execute or unlowerable calls become `deferred_tool_calls` (hints the model must re-issue). A terminal first call defers every extra call. `MultipleToolCalls` remains as a recoverable error variant but is not emitted.

`ExecutionDecisionEnvelope` carries the `Decision` plus `request_hover_discovery`, `request_vision` / `vision_reason`, `step_completed` / `step_failed`, `needs_plan_revision`, `task_state_action`, `deferred_tool_calls`, `thinking`, and raw payloads. Observation policy is action-aware (full / quick / DOM-only / skip); `request_vision` asks for a screenshot and does not imply coordinate fallback.

### Catalog assembly (`native_catalog.rs`)

`BUILTIN_LANE_NAMES` is empty; `files` / `http` / `shell` are compiled packs flowing through `direct_capabilities`.

1. **Direct pack capabilities** — the agent's `tools:` allowlist plus `UNIVERSAL_BACKEND_PACKS` (trusted). `trust_level == "untrusted"` gets only `SAFE_UNIVERSAL_PACKS_FOR_UNTRUSTED` (`activate_skill`, `deactivate_skill`, `time_math`). Catalog and dispatch ceiling stay in lockstep.
2. **Control tools** (autonomous only): `need_user_input`, `yield`, `read_result`; `spawn_sub_goal` when depth/budget allow.
3. **Delegation tools** when targets exist: `delegate_to_agent`, `handover_to_agent`.

Chat mode strips autonomous metadata (`task_state_action`, hover/vision, step signals) and terminates with assistant text, not `yield`.

One immutable **tool-policy snapshot** (`policy_snapshot.rs`) is resolved per provider decision boundary; catalog, dispatch ceiling, deferred index, delegation schema, and introspection all derive from it. A provider schema is never authorization by itself. Structural control names are assembled first, then whole-tool `denied_tools` exclusions apply, so controls cannot bypass pack policy.

Hot/deferred split, `ALWAYS_HOT_TOOL_NAMES`, and `tool_search`: [FLAT_LOOP.md](./FLAT_LOOP.md).

### Lowering (`native_lowering.rs`)

Dedicated arms: `yield`, `need_user_input`, `spawn_sub_goal`, `delegate_to_agent`, `handover_to_agent`, plus `goal_reached` / `cannot_proceed` aliases. **Everything else** (files, http, shell, search, fetch, `tool_search`, `browser`, `duckdb`, Yutori bare names rewritten to `browser__<action>`) goes through `lower_pack_execute` → `ExecutableAction::Pack`. The parser never produces `ExecutableAction::File` / `Http` / `Bash`; those variants are used only inside compiled providers.

Metadata sidecar fields are stripped before pack arguments reach capability code.

---

## Tool surface

`merged_agent_tools: Vec<ToolInfo>` (built by `MagicianV2Orchestrator::resolve_merged_agent_tools[_for_scope]`) is authoritative for planning and execution.

- **Direct ownership wins**: a tool the calling agent grants stays its own even if a delegate also lists it (avoids wasteful delegation hops). Among delegates, the first lexicographic agent id owns the projection.
- Delegate-owned tools carry `providing_agent_id`; the model should `delegate_to_agent` for them.
- Resume/continuation restores `merged_agent_tools` from the scoped pause context so a resumed run never continues with `0 tools`.

### Universal substrate (`UNIVERSAL_BACKEND_PACKS`)

Injected for trusted agents only.

| Group | Packs |
|-------|-------|
| Memory | `save_preference`, `update_memory_tier`, `search_memory`, `forget_memory`, `list_memory_tiers` |
| Introspection | `list_tasks`, `list_agents`, `inspect_agent`, `get_agent_details`, `find_agents_for_capability`, `list_artifacts`, `list_scheduled_tasks`, `list_episodes`, `list_proposals`, `get_active_executions`, `get_execution_history`, `read_program_state`, `read_trace`, `system_status`, `time_math` |
| Task ops | `stop_task`, `update_task`, `delete_task`, `refine_task` |
| Surfaces | `create_dashboard`, `unpublish_dashboard` |
| Identity / skills | `switch_personality`, `activate_skill`, `deactivate_skill` |
| Meta / files | `tool_search`, `edit_file`, `glob`, `grep`, `files`, `read_file`, `write_file` |
| HTTP / shell | `http`, `shell` |
| Acquisition | `content_search`, `content_read`, `web_fetch`, `web_search`, `web_answer` |

`task_state` is injected only when `ctx.task_id` is set. Admin packs (`create_agent`, `treasurer`, `notify_owner`, `create_proposal`, …) are not on this rail.

### Progressive disclosure

Each iteration the focused capability gets its full pack spec in the DECIDE prompt; the rest get compact name+description+parameter summaries. Unfocused browser defaults live in `skillshub/browser/SKILL.md`.

---

## Decisions and outcomes

`Decision` (`decision.rs`):

| Variant | Source |
|---------|--------|
| `Execute { candidates, thinking }` | Work tool call (always a `CandidateBatch`) |
| `Yield { payload }` | LLM terminal; `dispose_yield` classifies Completed / PartialSuccess / Failed / RetryTransient |
| `NeedUserInput { … }` | Pause primitive (not terminal) |
| `SpawnSubGoal { goal, unblocks, budget_iterations }` | Inline nested loop |
| `DelegateToAgent { targets }` | Isolated child executions (or in-context single-target) |
| `HandoverToAgent { … }` | Same-execution owner swap |
| `Completed` / `Failed` | Synthetic fallbacks only (text-without-tool, malformed args) |

`AgenticOutcome` (`types.rs`): `Success`, `Failed`, `MaxIterationsReached` and `BudgetExhausted` (pausable when `pause_state` is `Some`), `LoopDetected` (stored outcomes only; live loop pressure is advisory), `WaitingForUser`, `WaitingForConfirmation`, `PausedByUser`, `WaitingForChildren`, `CannotProceed`, `Sleeping`.

`UserInputType`: `Text`, `Password`, `Choice`, `MultiChoice`, `Confirmation`, `ExternalAction`, `FilePath`, `Guidance`, `ToolAuthorization`, `SandboxOverride`.

User elicitation is a first-class decision, not an error: `NeedUserInput` → `WaitingForUser` (workflow PAUSED) → user responds via API → `FullPauseStore.resume(user_input)`. `WaitingForConfirmation` is the sibling pause for destructive actions (`action_summary`, `reason`, `action_type`, `action_json`, optional secret-approval challenge). `Sleeping` persists state and schedules a `WakeUpQueue` timer.

---

## Dispatch

`CapabilityRegistry` holds `CapabilityProvider`s; lowering always emits `ExecutableAction::Pack`.

| Pack kind | Provider / dispatcher |
|-----------|------------------------|
| Compiled filesystem / HTTP / shell | `FileCapabilityProvider` (`files`, `read_file`, `write_file`), `HttpCapabilityProvider`, `ShellCapabilityProvider` (`compiled_providers.rs`); internally execute `File` / `Http` / `Bash` |
| Compiled search | `SearchCapabilityProvider` |
| Compiled task state | `TaskStateProvider` (when `ctx.task_id` is set) |
| YAML `composite` / `command` | `PackCapabilityProvider` (`pack_provider.rs`) |
| YAML `primitive` | `primitive_dispatch` (`dispatch.rs`): browser → pinned `agent-browser` CLI; DuckDB etc. keep Rust providers; CLI packs use `ScopedDeterministicCapabilityInvoker` |

`ImplementationType` (`capability.rs`): `Composite { steps }`, `Compiled { provider_name }` (metadata only), `Primitive { … }`, `Command`. `PackCapabilityProvider` uses the YAML definition's implementation type, not a placeholder on the parsed action, and re-resolves params (required, aliases, defaults) even for raw DECIDE-path actions.

Command-pack launch: omitted optional params are dropped before argv/env construction; missing required params fail before launch; aliases resolve to the canonical field so `arg_mappings` and env templates stay stable; nested `body="{...}"` JSON is recovered for known declared fields.

### Browser

Browser is a primitive pack (`BROWSER_PACK_NAME = "browser"`); the model calls `browser__<action>` leaves and `dispatch_browser_primitive` drives the pinned agent-browser CLI.

`connection_mode` is a **request**, not authorization. Resolution: retrieval-handoff exact mode → call `connection_mode` → `MAGICIAN_AGENT_BROWSER_MODE` → default `cdp`. The result is held to the **owner agent's** `browser_transports` ceiling (empty = `cdp` / `headed` / `headless`) in `BrowserTransportCeiling::resolve`. Out-of-ceiling transports are **refused**, never substituted. The ceiling is replaced, not merged, at every owner transition.

Browser failures return to the visible agent loop: no hidden mode fallback, no coordinate-click retry rail. `AgenticClickFallbackUsed` exists only for old clients.

---

## Budgets

| Dimension | Where | Default |
|-----------|-------|---------|
| Iterations | `AgenticContext.max_iterations` (`with_max_iterations` overrides) | 4000 |
| Wall-clock work | `work_budget_secs` | `None`; active time is cumulative across pause/resume, paused time excluded |
| USD cost | `MAGICIAN_AGENTIC_MAX_COST_USD` via `agentic_max_cost_usd()` | `$15` when unset; `0` / non-positive disables. Checked before new work after a decision |
| Tokens | `max_tokens_per_cycle` | `None`; `MAGICIAN_STRICT_TOKEN_USAGE` (off) fail-closes on missing usage metadata |
| Repeated actions | `max_repeated_actions` | 3 |

There is no `max_llm_calls` field (`BudgetDimension::LlmCalls` is retained on the enum). On budget reach an in-flight op may finish; no new model turn, tool, or delegation starts; completed findings go through `goal_achieved_partial` collation.

Delegation `timeout_secs` is a **soft work budget**, not a cancellation timer. Explicit value wins; else `normal` / `deep` / `thorough` = 300 / 900 / 1800 s; else no delegation budget is invented. The source agent's `delegation_timeout_secs` never clamps it. Invalid budget/depth fails the all-or-none preflight.

Strategy exploration budgets (`strategy/types.rs`) bound planning/search, not the loop: 100 LLM calls, 2 h, 80k tokens, confidence 0.7.

---

## Loop detector

`loop_detector.rs` fingerprints state and actions over the last 20 records (`check_before_action`). Detection **appends advisory progress-pressure** to history; the model decides whether repetition is valid. Hard iteration/time/cost budgets are the emergency stop.

| Kind | Rule |
|------|------|
| `StateLoop` | similarity > 0.95 (`STATE_SIMILARITY_THRESHOLD`), seen ≥ 3 |
| `ActionCycle` | repeating A→B→A→B |
| `NoProgress` | state unchanged after actions, similarity > 0.90, ≥ 5 |

Browser fingerprint weights (total 12.5): URL 3.0, DOM hash 2.0, content hash 2.0 (skipped if empty), scroll bucket 2.0 (same 5% bucket full, adjacent partial), interactive count 1.0 (±2), modal / error 1.0 each, title hash 0.5.

---

## Step-stuck warning and adversarial reviewer

### `AgenticStepStuckWarning`

Fires once `consecutive_no_action_iterations >= 3` (`STUCK_WARNING_THRESHOLD`, `run_loop/phases/epilogue.rs`) and re-fires until a successful tool call resets it. Observability only (a `[STUCK-WARNING]` log plus the event) — never skips, aborts, or rewrites an action.

`is_preflight` (default false) lets consumers ignore historical pre-loop probe emissions. Task bootstrap and resume must not run catalog-wide capability probes (`executor.rs` guards this); readiness diagnostics live in `preflight.rs` for setup surfaces only.

### Adversarial progress reviewer

A task-backed **read-only streak** reaching `NO_PROGRESS_AUTO_YIELD_THRESHOLD` (10, `decision.rs::detect_stuck_kind`) runs one in-execution reviewer (`run_adversarial_reviewer`, operation `progress_review`) instead of hard-concluding:

- `continue` / `converge` / `redirect` → inject a `[progress-review]` steer and decide normally
- `escalate` → `try_synthesize_stuck_auto_yield`

Firing is stateless from streak length: every 3 iterations after 10 (10/13/16/19), hard-conclude at 20. Repeat-churn (`NO_PROGRESS_REPEAT_AUTO_YIELD_THRESHOLD = 3`) and non-task-backed runs still hard-conclude. A lighter prompt nudge (`STUCK_SIGNAL_THRESHOLD = 3`, `STUCK_AUTO_YIELD_THRESHOLD = 5`) fires before orchestrator auto-yield. Telemetry target `agentic.reviewer`.

---

## Pause and resume

`NeedUserInput` → `WaitingForUser`; `WaitingForConfirmation` for destructive actions; `PausedByUser` needs no new input. `AgenticPauseState` carries iteration, environment, history summary, tool scope, budgets, owner stack, and routing overlays; resume resolves `merged_agent_tools` from it.

### Shared direct-agent override builder

Chat-inline delegation, delegated child execution, and task-backed direct execution (initial and resume) share one override path: resolve the owning agent once, derive `merged_agent_tools` / `allowed_action_types` / `denied_tool_params`, keep per-operation `llm_routing` overrides, then call `execute_agentic_direct_with_outcome()`. Task-only controls (output mode, pipeline stages) layer on top. Why: separate paths drifted into partial tool surfaces and wrong routing profiles.

A caller-supplied execution route is written to `executions/<execution_id>/llm_routing_overrides.json`, durably copied to each delegated child before launch, and reloaded on every reconstructed binding and resume. Hot paths read it with known scope; execution-id discovery is only for legacy scope-less callers. The sidecar never mutates the task manifest or agent definition.

Runtime context keeps that immutable overlay separate from refreshable owner defaults, recomputes `owner + execution` routing at every owner boundary, and derives decision/critic/compaction adapters from the merge at call time. Pause state persists both projections.

---

## Spawn sub-goal

`spawn_sub_goal` runs **inline** in the parent loop (no new Task). Requires non-empty `goal` and `unblocks`. Budget default 75 (`DEFAULT_SUB_GOAL_BUDGET`), clamped to 1..=400, then `min(budget, parent remaining − 1)`; zero rejects, and the tool is withheld when remaining iterations cannot fund it.

`ctx.depth >= max_delegation_depth` (default 2, `constraints.coordination.max_delegation_depth`) → `DepthLimitReached`. A goal with too-high trigram overlap with the parent → `SubGoalParaphrasesParent` (failed iteration, not a loop). Approval rules are inherited (no privilege escalation); parent-specific state is cleared; depth increments. The nested future is heap-boxed; artifacts roll up to the parent history.

---

## Delegation

Runtime tools: `delegate_to_agent`, `handover_to_agent` (no `create_task` on this loop).

- **Chat-inline `delegate_to_agent`** skips GuidedSearch / AtomicComposition, has no execution-mode switches, and returns after starting the child; progress and the terminal summary arrive on the `chat` progress channel carrying delegated execution/thread context. Inline delegates are task-backed and inherit `(principal, workspace, ui_thread_id)`.
- **`handover_to_agent`** is same-execution ownership transfer. Requires `preserve_live_execution_context=true` **and** a real runtime continuity anchor (active browser/app/session); prompt wording or shell telemetry is not enough. Inside an existing same-execution specialist chain it must yield back or delegate instead.
- **In-context single-target** chat `orchestrate_pipeline` hops: [IN_CONTEXT_DELEGATION.md](./IN_CONTEXT_DELEGATION.md). Multi-target and non-pipeline delegations spawn children; the parent enters `WaitingForChildren`.
- **Typed named-delegation launch**: a V3 execute request may name one `delegate_to_agent`; policy, cycle/depth, idempotency, admission, and isolation still apply — only the root-model routing decision is skipped. Admission locks are keyed by parent execution.
- **Idempotent reconciliation**: targets are canonicalized and fingerprinted with the parent execution. Equivalent targets in one response collapse to one child; retries/resumes reconcile existing children; failed/cancelled attempts may create a new retry revision. A replay resolving only to completed children stays in the loop and must not enter `WaitingForChildren`.
- **Post-delegation**: V3 orchestrator owns only `WaitingChildren → Runnable`; Artifact V2 owns the continued parent. The next prompt includes child deliverables and asks the parent to conclude, delegate a named remaining gap, or yield — not restart the research. Continuation is handed to a lazy execution-runtime job so the parent never nests under the still-unwinding child executor. Reconciliation is capped at four decisions. A first-run Markdown/plain-text task-agent projection is copied deterministically from execution output when audience and evidence match; other cases use normal synthesis.
- **Exactly-once terminal synthesis**: one deterministic primary-output revision per execution. Completion, retry, and startup recovery share one claim set; children have one child-terminal single-flight owner. A crash mid-provider-request recovers at least once but commits one output.
- **Tree-wide cost identity**: every task-backed provider call (root, children, resumes, synthesis, progress review, learning reflection) carries the same `root_execution_id`; the scoped `llm_calls` ledger is the money source.

---

## Task state, artifacts, and research evidence

`task_state` is a compiled capability persisted via `DurableArtifactStore` (namespace `"task_state"`), preloaded into `AgenticContext.task_state`, rendered as `{task_state_section}`, and carried on `AgenticPauseState`. `task_state_action` is optional **inline metadata** on the chosen tool call (default `none`, strictly validated when present); the create/patch/close contract lives in the decision prompt.

After a successful file write/append/copy inside the durable store, `try_register_durable_file_action()` stamps `source_task_id` (temp file + atomic rename) and records it in `durable_artifacts_written`. Linked-artifact enrichment: [LINKED_TASK_ARTIFACTS.md](./LINKED_TASK_ARTIFACTS.md).

Taskplan spills are a **storage optimization**, not control flow: critical outputs stay in recent history; spill artifacts are promoted to the scoped store; reads of spills never re-spill and share one loop-detection family.

`content_search` / `content_read` accept scalar requests or a `requests` array for fan-out. The bounded vector `content_search` projection keeps every admitted candidate's URL and snippet as a discovery-only record (`claim_eligible: false`), so an agent can pass the URL straight to `content_read` without paging the separately materialized raw envelope. Read evidence exposes requested/final URL, redirect state, title, and excerpt; `fetch_status` is transport completion only.

Output synthesis preserves evidence order: a later direct success supersedes an earlier failed/indirect attempt for the same claim; a failure stays visible only if it leaves a material gap. Synthesis may format or compress but not add unsupported claims.

**Research precision review**: before publishing a completed/partial terminal draft, one governed review checks it against only successful `content_read` records marked `opened_page` and `claim_eligible`. Faithful → earlier attempts closed as superseded; unsupported → one correction turn with the critic's gap; second unsupported → fail closed; critic outage → fall back to the deterministic terminal gates (an auxiliary service must not make the runtime unavailable).

---

## Decision metadata and rationale

Schemas advertise one optional open `decision_metadata` object (fields defined in the prompt); lowering maps it into the envelope. Historical flat values are accepted, equal dual values accepted, conflicting dual values fail closed. Chat schemas strip it.

`thinking` is a concise action rationale, capped at 240 chars (`MAX_DECISION_RATIONALE_CHARS`); omission falls back to a tool-name label; folded turns keep per-candidate rationales.

Native responses are captured **before** lowering (assistant text, tool-call ids/names/args, finish reason, tokens). `ExecutionHistory.format_for_llm(...)` renders a bounded recent assistant-turn section so the next decision sees the visible intent chain even when `task_state_action` is `none`. Hidden reasoning is never stored.

`## CURRENT STATE` is rendered by `EnvironmentState::format_for_llm_with_replayed_result(iterations_in_messages > 0)`. When the last result is already in the replayed native messages, shell/pack states reference it ("delivered whole as the tool result above (N bytes)") instead of pasting a truncated copy — a duplicate costs tokens and a "(truncated)" marker misleads the model into thinking rows were lost. Browser, filesystem, and http states are the observation itself and render unchanged.

---

## Provider context reuse

- **OpenAI Responses** and opted-in **Gemini Interactions**: one full bootstrap, then only deltas via the provider checkpoint id (tools/system/generation controls resent where Gemini requires).
- Clean bounded **re-bootstrap** after six continued turns, or immediately when provider/model/endpoint/API mode, image shape, compaction, resume hydration, or assistant anchoring makes the chain unsafe. A checkpoint is never combined with replayed pre-checkpoint history.
- **Anthropic, OpenAI Chat, Gemini `generateContent`, MiniMax, DeepSeek, OpenRouter**: full bounded transcript with provider prefix caching.
- **Ollama, Yutori, unknown/custom**: bounded replay, no cache contract.

A local fingerprint (model, tool order, system prompt, sentinel-delimited stable user prefix) is never sent upstream.

---

## Realtime events

| Event | Trigger | Notable fields |
|-------|---------|----------------|
| `AgenticExecutionStarted` | Loop begins | `goal`, `success_criteria`, `max_iterations`, optional `hint_action` / `agent_id` |
| `AgenticIterationStarted` | Each iteration | `iteration` (1-indexed), `environment_type` |
| `AgenticIterationCompleted` | End of iteration | `duration_ms`, `outcome` (`action_executed` / `decision_rejected` / `loop_continue` / `terminal` / `paused` / `cancelled`) |
| `AgenticDecisionMade` | LLM decides | `decision_type`, `action_summary`, `thinking`, `tool_name`, `candidates_count`, `raw_decision` |
| `AgenticActionExecuted` | Action completes | `action_type`, `target`, `success`, `latency_ms`, `error` |
| `AgenticStepStuckWarning` | No-success streak ≥ 3 | `consecutive_count`, `recent_actions` (≤3), `is_preflight` |
| `AgenticPageUnderstanding` | Multimodal page understanding | `observation_id`, `page_stage`, `element_count`, `has_screenshot` |
| `AgenticExecutionCompleted` | Loop ends | `outcome`, `iterations_used`, `artifacts`, `duration_ms`, `summary` |

`AgenticExecutionCompleted.outcome`: `success`, `failed`, `max_iterations_reached`, `loop_detected`, `waiting_for_user`, `waiting_for_confirmation`, `budget_exhausted`, `cannot_proceed`. HITL payloads ride on `HitlRequested { source: "agentic" }`, not the slim `AgenticWaitingFor*` markers. Full tables: [v2-websocket-events.md](../v2-websocket-events.md).

---

## LLM routing

Static operation/profile routing, in order: `llm.router.operation_mapping` profile per operation → optional `when_has_images` alternatives → authorized request/agent overrides → pinned provider/profile locks for fail-closed local-only operations → configured fallback/retry → chat-only fast/thinking profiles via `request_thinking_mode`. No live complexity classifier; see Adaptive LLM Routing.

---

## Scope-loss guards

A task-backed execution lives at `{scope}/tasks/{task}/executions/{exec}/execution.json` plus the artifact dir, resolved by `resolve_runtime_execution_dir`; if gone, writes fail with `missing_v3_execution_scope`. The scope can vanish mid-run (`delete_execution` is `remove_dir_all`; a terminal → `Executing` transition can revive a reclaimed id). Guards:

1. **Resumer self-terminate** (`trigger_goal_awaitable`): pipeline `Completed(Ok)` plus a permanent status error (`not found` / `missing_v3_execution_scope`) marks the task Failed; transient errors retry.
2. **Loop scope-liveness** (`execute_agentically`): from iteration 2, `Ok(None)` from the resolver aborts and finalizes Failed (`[AGENTIC-SCOPE-LOST]`); transient errors never abort.
3. **Cancel-before-delete**: `delete_execution` cancels first so the loop stops at the next seam; loop-entry `get_execution` fails closed on late revivals.

---

## Delegation continuation: active-root reclaim

A delegated child **shares its parent's `task_id`**. On child terminal, `reconcile_waiting_children_and_continue` → `continue_task_backed_parent_after_child_completion` → `activate_execution(scope, task_id, parent_execution_id)` re-claims the task root.

`activate_execution` is an atomic check-and-swap under the process-global `active_root_swap_gate` (`artifact_v2/service.rs`). It reclaims only a **definitively terminal** holder (`completed` / `failed` / `cancelled`) or `ExecutionNotFound`; live, paused, or waiting holders block; transient/corrupt reads fail **closed**. The reducer writes `active_root_execution_id` under a different lock, so a lost update can strand a root but **cannot produce two live roots** — only root executions write that field and reclaim is terminal-only.

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
| Prompt pins / decision prompt | `magician-core/src/prompts/constants.rs`, `data/magician_v2/prompts/agentic_decision_v1.3.7.json` |
| Browser skill | `skillshub/browser/SKILL.md` |

Merkle helpers in `execution/merkle.rs` / `types.rs` remain only for historical `PageState` records; they are not the browser control path. Set-of-Mark observation is retired.

---

## Multi-Step Orchestration Patterns

All execution paths converge on the direct loop; a stored `PlanGraph` is lowered for validation, rendered into runtime context (deriving goal, success criteria, initial URL), then run by the same `execute_agentically()`. There is no pipeline step executor; multi-action structure comes from taskplan guidance and the model's tool choices.

```
ctx = AgenticContext::new(goal, success_criteria)   // max_iterations = 4000
        .with_max_iterations(n)
        .with_observability(execution_id, plan_id, step_id)
match execute_agentically(&ctx, state, executors).await:
  Success                                → complete
  WaitingForUser/WaitingForConfirmation  → pause, collect input, resume
  WaitingForChildren                     → wait, reconcile parent
  Sleeping                               → schedule wake
  Failed/BudgetExhausted/scope lost      → fail (budget may be resumable)
```

4000 is the agent-routed safety ceiling; focused tasks should set 4–8. Split a step in the **taskplan** when many UI actions, likely branching, intermediate verification, or major page-context changes (navigation, login, modal flow) are expected.

### What a scoped run needs before the default driver will start it

The stateless arm refuses rather than falling back to the resident loop. A run needs all four:

- **Execution id, principal, and workspace** on the context (`StatelessArm::new` keys loop state by them and names the missing one).
- **A sealed legacy-writer cutover** for that scope (`test_support::build_test_artifact_v2_harness` seals conventional test scopes).
- **An `OwnershipRuntime`** on the executors; its trait defaults refuse rather than guess.
- **A real Artifact task and execution** to settle against; the settle refuses an incomplete agent scope, so the owner is named on the context and, on resume, in the pause snapshot (`AgenticPauseState::with_agent_routing`).

Consequences:

- A context with `task_id` is **task-backed**, and a task-backed completion yield without material evidence is refused.
- Every dispatch needs an effect id (LLM call id + tool-call id); without it the loop refuses to fire, since no restart could prove whether it already happened. Scripted decision fixtures must carry a trace receipt and distinct tool-call ids.

Operator controls differ by arm (reading the resident-loop one on the stateless arm reports a live run as inactive):

- **Steer** goes to the durable inbox (`run_loop::steer_inbox`), claimed against the sealed control generation, not the process-local `steer_queue`. Admission requires the runtime row to be `Executing`.
- **Manual resume** uses `claim_stateless_manual_resume` under a sealed tree transaction; `take_manual_pause_for_execution_with_recovery` is the resident path and takes the arm as an argument so the choice stays with the caller.

Reference composition: `magician/tests/driver_phase_differential.rs` (one script under both arms); `executor.rs`'s `scoped_loop_run` for in-crate tests. The scoped path is deeper on the stack; stack-fit tests keep tokio's default 2 MiB worker stack because production does.
