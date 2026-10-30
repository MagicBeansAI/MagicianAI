# Harness Cycle Continuation Decisions

Owner: harness lane.

## What it is

Every harness cycle settles on exactly one continuation decision, recorded
durably with the budget that justified it.

`harness_cycle_dispatch_outcome` says whether a cycle *ran*; the continuation
decision says what it chose next, so a harness waiting on its owner and one
that finished stay distinguishable after the fact, each with its cost.

## The five decisions

| Decision | Meaning | Terminal for the cycle |
|---|---|---|
| `continue` | Work remains and the harness may take it up | no |
| `wait_for_evidence` | Blocked on evidence arriving from elsewhere | **yes** |
| `request_approval` | Blocked on an owner decision | **yes** |
| `retry_later` | Failed or out of budget in a way a later attempt may survive | no |
| `stop` | Finished, or stopped in a way no later attempt should reopen | **yes** |

`is_terminal_for_cycle` and `permits_autonomous_iteration` are exact
complements, asserted per variant, so no decision falls through the rule.
Unrecognised text is **refused** by `from_wire` rather than coerced — a
decision nobody defined must not silently become permission to keep running.

## Exactly one decision per cycle

The decision is recorded at the single post-cycle funnel every harness cycle
passes through. A cycle that states nothing gets one inferred from its outcome:

| Episode outcome | Inferred | Why |
|---|---|---|
| `goal_achieved` | `stop` | the goal was achieved |
| `partial_progress` | `continue` | progress was made and work remains |
| `paused` | `request_approval` | pending actions await a decision |
| `user_intervened` | `stop` | a person took over the lane |
| `budget_exhausted` | `retry_later` | a fresh cycle gets a fresh budget |
| `failed` | `retry_later` | a later attempt may survive it |
| `circuit_open` | `stop` | hammering an open breaker burns budget |
| anything unrecognised | `retry_later` | never `continue` — no autonomous continuation is assumed |

Inference makes "every cycle persists a decision" an invariant. The stated
path stays open for a cycle that names its own reason; nothing fabricates one
from prose.

## Budget honesty

Elapsed time, retries and pending actions are measured at the funnel. Tokens,
cost and open loops are optional and report **absent, not zero** — the
rationale reads "tokens unmeasured" rather than implying a free cycle. Token
telemetry does not reach this boundary yet, and a `0` there would be a
fabrication.

## What a decision does not grant

`last_cycle_decision` lands in `ProgramRuntimeState` through the existing
bookkeeping allowlist, and is deliberately **not** a control field. A cycle
narrating "I am waiting on approval" describes itself; it must not thereby
acquire the power to pause its own lane.

Nor does a self-reported decision suppress future scheduled cycles. The
terminal rule means *this cycle ends and the harness takes no further iteration
on its own authority*; gating a cron lane on the agent's judgement of itself
would let a harness wedge itself off. Pausing a lane stays the owner's act,
through the pause index that drops a dispatch.

## Fires that never run

A continuation decision only exists for a cycle that ran. Every cron fire that
ends *before* running still records a `harness_cycle_dispatch_outcome`
(`harness/cycle_outcome.rs`):

| Outcome | Cause |
|---|---|
| `dropped_globally_paused` | `harness.paused` / `MAGICIAN_HARNESS_PAUSED` is on |
| `dropped_paused` | this agent is paused |
| `dropped_definition_stale` | definition missing or changed since the fire was prepared |
| `dropped_coalesced` | the agent/goal already has active or queued work |
| `dropped_duplicate` | this `(agent, goal, trigger_seq)` was already admitted, or an episode already records it (`stage: before_provision` when decided before a task/execution exists, `dispatch` when an already provisioned execution had to be cancelled) |
| `dropped_queue_full` | the runtime trigger queue is at capacity |
| `dropped_provision_failed` | task/execution provisioning failed, or the process-wide scheduled-root capacity was exhausted |
| `dropped_disabled` | the agent is disabled in the delegation hierarchy |
| `dropped_reservation_changed` | the active reservation changed before launch |
| `dropped_no_execution_id` | launched, but no runtime execution id was bound to the reservation |

Recording them keeps a paused loop distinguishable from a scheduler that never
fired. None opens an anomaly: anomalies are classified only for cycles that
reached the post-cycle pipeline.

A cycle that *does* run but whose focus area names a program document that is
missing or empty records `harness_program_missing` and gets a `## Program
Context` block saying so, instead of silently running with no program, state or
metrics.

## Key files

- `harness/cycle_continuation.rs` — the enum, the budget snapshot, the
  derivation, and the durable record.
- `harness/program.rs` — the `last_cycle_decision` runtime-state field.
- `learning/program_state_bridge.rs` — bookkeeping allowlist entry, with the
  test pinning that it is not a control field.
- `magician-api/src/web_api.rs` — the post-cycle funnel where each decision is
  settled (`decide_continuation` + `record_cycle_continuation`).

The full boundary plan is [Bounded Research and Harness
Adaptation](../../archive/plans/2026-08-09-bounded-research-and-harness-adaptation-plan.md).
