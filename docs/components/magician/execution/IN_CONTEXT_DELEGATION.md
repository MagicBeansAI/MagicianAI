# In-Context Delegation (single-execution chat pipelines)

**Related Docs**:
- [AGENTIC_EXECUTION_DESIGN.md](./AGENTIC_EXECUTION_DESIGN.md) — the observe-decide-execute loop this plugs into
- [FLAT_LOOP.md](./FLAT_LOOP.md) — the flat decision loop
- History / decision record (archived): `docs/archive/plans/2026-06-04-step3-stage-source-fork.md`

---

## Overview

A chat **`orchestrate_pipeline`** task (a multi-capability ask like "research X → ELI5 it → make an 8-panel comic") is driven by the **PA-root agent's dynamic loop**, which decides the ordered stages at runtime, one `delegate_to_agent` decision at a time.

For these tasks a **single-target** `delegate_to_agent` runs **in-context on the one `ExecutionRun`**: the runtime swaps the active owner to the target agent, runs it inline, swaps back, and threads its deliverable into the PA root's next decision, **instead of spawning a child `ExecutionRun`** and resuming the parent via reconcile. Multi-target (parallel fan-out) and all non-pipeline delegations keep the spawn-child path.

## Why (the problem this solves)

A child `ExecutionRun` per stage makes the parent suspend (`WaitingForChildren`) and later rebuild its context **from disk**. Each rebuild fills what it can and defaults the rest, so a broken contract surfaces far from its cause (a `missing_v3_execution_scope` on a synthetic stage id, a dark chat card, a dropped deliverable). One execution removes that boundary: writes land on the one registered execution, events flow to one chat card, and deliverables collect naturally, while the PA root keeps its adaptive loop.

## Mechanism

All in `magician/src/magician_v2/execution/agentic/executor.rs` unless noted.

1. The PA-root loop emits `Decision::DelegateToAgent { targets }`.
2. `handle_delegate_to_agent_decision(ctx: &mut AgenticContext, …)`: after validation and the approval gate, if `targets.len() == 1 && ctx.delegate_single_in_context` and an `ownership_runtime` is wired, it calls `run_single_delegate_in_context(...)`; otherwise it falls through to the spawn-child path (never a silent skip).
3. `run_single_delegate_in_context`:
   - **`StageGuard::push_delegate_frame`** swaps the active owner to the target via `transition_execution_owner`, loading its full `OwnerExecutionProfile` (persona, scoped tools, trust). The delegating owner **stays on `owner_stack`** (see [Delegation guards across the hop](#delegation-guards-across-the-hop)); `finish()` pops it. Pipeline stages use plain `StageGuard::push`, which pushes an empty stack, because stages are siblings, not nested frames.
   - Builds `sub_ctx = ctx.clone()` **after** the push (so it inherits the swapped persona/tools/trust), with `goal = target.context`, `depth + 1`, and the **ROOT `execution_id`** (no synthetic `{root}::stage{n}` ids).
   - Runs **`Box::pin(execute_agentically(&sub_ctx, …))`** inline; `Box::pin` breaks the async-recursion cycle that would otherwise be an infinitely-sized future (E0733).
   - Calls **`StageGuard::finish` UNCONDITIONALLY** before propagating any error (the **C2** invariant), restoring the PA-root owner so the persisted owner snapshot cannot leak to the sub-agent.
   - Extracts the text deliverable from the returned `AgenticOutcome` (`delegate_outcome_primary_text`, mirroring the orchestrator's `primary_text_from_outcome`), renders it with `render_child_deliverables_block` and appends it to **`ctx.prior_environment_knowledge`**, which `build_decision_prompt_from_manager` clones into `environment_knowledge_section` (decision.rs) for the **next** decision. This is the reconcile path's vehicle: no new prompt injection, no `ExecutionHistory` schema change, no `ExecutePathControl` payload.
   - Returns `ExecutePathControl::Continue`. The loop's delegate `Continue` arm **reloads `trust_dispatch_guard`** (as the handover arm does) so the next iteration enforces the restored PA-root trust.
4. The PA root's next iteration sees the deliverable and decides the next stage (or completes).

## Delegation guards across the hop

An in-context hop swaps the execution's active owner without creating a child
`ExecutionRun`. Three delegation guards read the ownership record rather than
the child row, so the hop has to be visible there or all three stop applying at
once. `ExecutionRun.owner_stack` — "the owners suspended beneath the current
one on this execution" — is what carries it:

| Guard | Where | What it reads |
|---|---|---|
| Transitive-delegation gate | `validate_delegation_request` (`executor.rs`) | `ctx.owner_stack` non-empty ⇒ the sub-agent must set `coordination.allow_transitive_delegation` to sub-delegate. Default is `false`. |
| Same-execution cycle guard | `validate_delegation_request` (`executor.rs`) | A target already on `ctx.owner_stack` is an `A → B → A` re-entry into an owner that is still waiting for this frame — rejected as `CircularDelegation`, on both the in-context and the spawn-child branch. |
| Durable depth + cycle accounting | `inherited_delegation_chain` / `delegated_child_admission_error` (`agents/runtime.rs`) | The chain a spawned child inherits is `delegation_chain ∪ owner_stack ∪ {active owner}`, so same-execution hops count toward `max_delegation_depth`, arm the durable transitive gate, and make `delegation_chain.contains(target)` reject a spawned cycle. Depth is that chain's length minus the active owner: a root is still 0. |

Because the sub-agent runs with a non-empty `owner_stack`:

- **It must not yield back.** `yield_back_to_previous_owner` fires for handover
  specialists on `owner_stack`, but the frame that pushed an in-context delegate
  owns the restore (`StageGuard::finish` inline, or the continuation-frame
  unwind after a pause). `running_as_in_context_delegate` (true when any
  continuation frame is an `InContextDelegation`) suppresses the yield-back so
  the owner is restored exactly once, including for a same-owner sub-goal nested
  inside the delegate; a sub-goal under a plain handover specialist is
  unaffected.
- **It cannot hand over.** `HandoverChainNotAllowed` (handover requires an empty
  `owner_stack`) applies inside an in-context delegate, which is a leaf its
  parent frame waits on. It can still `delegate_to_agent` if allowed.

## Scoping (deliberately NOT global)

A `bool` on `AgenticContext`, **`delegate_single_in_context`** (+ an `AgenticContextOverrides` mirror, applied in `v2_orchestrator.rs`), gates the swap. The service sets it `true` **only** for chat `orchestrate_pipeline` tasks, and only when `config::pipeline_single_context_enabled()`. So:
- generic single delegations system-wide use spawn-child (pinned by `test_spine_delegate_to_agent_yields_waiting_for_children`);
- multi-target / parallel fan-out always spawns children;
- a sub-agent clones the flag, so nested single delegations within a pipeline stay in-context too (bounded by `depth`).

Not widened beyond `orchestrate_pipeline`: much larger blast radius with no concrete need.

## Flags

- **`MAGICIAN_ORCHESTRATE_PIPELINE_SINGLE_CONTEXT`** — default **ON**. Enables in-context single-delegation for `orchestrate_pipeline` tasks (Option B, the live path). Opt out with a falsy value to force the legacy spawn-child loop.
- **`MAGICIAN_ORCHESTRATE_DECOMPOSE`** — default **OFF**. Opt-in for the **dormant** up-front-decompose engine (see below).

## The dormant decompose engine (retired Option A)

This design decomposes the goal into a fixed stage roster up front (`decompose_orchestration_goal`) and runs it on one `ExecutionRun` with per-stage owner swaps (`execute_pipeline_single_context` + `StageGuard`). It is **kept callable but dormant** behind the default-OFF `MAGICIAN_ORCHESTRATE_DECOMPOSE` flag, because a fixed up-front plan fails for adaptive agents locked into a frozen roster.

It checkpoints continuation boundaries durably. When a stage produces `WaitingForUser`, confirmation, manual pause, delegation wait, sleeping, or a resumable iteration/budget stop (`requires_continuation()`), the engine records the paused stage and its resume segment against the sealed roster (`mark_pipeline_stage_paused`, `artifact_v2/service.rs`) before returning, so resume continues the remaining roster. Crash recovery adopts an already-committed stage terminal before redispatching (source-lock tests in `v2_orchestrator.rs`).

Every stage writes under the root execution, so its artifact index is cumulative. The engine snapshots the index before each attempt and promotes only the post-attempt delta; a retry takes a fresh baseline, so earlier-stage and failed-attempt artifacts do not leak into the accepted deliverable. Spawned-stage join errors and task panics are held opaque until `StageGuard::finish` has restored the root owner.

## Resilience (effect over method)

A delegated sub-agent's terminal outcome is judged by **effect, not status**: any terminal outcome is accepted and whatever written deliverable exists is threaded, so a research sub-agent that did real work but ended non-`Success` still contributes. The no-progress detector does not count productive `*_search` / `*__run` iterations as read-only re-inspection (`iteration_is_read_only`).

## Key files

| Concern | Location |
|---|---|
| In-context runner + outcome text | `execution/agentic/executor.rs` — `run_single_delegate_in_context`, `delegate_outcome_primary_text` |
| Delegate decision handler + gate | `execution/agentic/executor.rs` — `handle_delegate_to_agent_decision` |
| Owner-swap RAII guard | `execution/agentic/executor.rs` — `StageGuard::push` (pipeline stages) / `StageGuard::push_delegate_frame` (nested in-context hop), over `transition_execution_owner` |
| Delegation guards | `execution/agentic/executor.rs` — `validate_delegation_request` (transitive gate, `CircularDelegation`), `running_as_in_context_delegate`; `agents/runtime.rs` — `inherited_delegation_chain`, `delegated_child_admission_error` |
| Threading render | `execution/execution_summary.rs` — `render_child_deliverables_block`; consumed at the decision seam (`decision.rs`, `environment_knowledge_section`) |
| Scope flag | `execution/agentic/types.rs` — `AgenticContext.delegate_single_in_context` (+ overrides); applied in `orchestrator/v2_orchestrator.rs` |
| Flags | `config.rs` — `pipeline_single_context_enabled`, `orchestrate_decompose_enabled` |
| Task gating | `artifact_v2/service.rs` — `execute_task_with_orchestrator` |
| Dormant decompose engine | `orchestrator/v2_orchestrator.rs` — `execute_pipeline_single_context`, `decompose_orchestration_goal` |
