# Harness Cycle Continuation Decisions

Owner: harness lane.

## What it is

Every harness cycle settles on exactly one continuation decision, recorded
durably with the budget that justified it.

`harness_cycle_dispatch_outcome` already said whether a cycle *ran*. It said
nothing about what the cycle decided to do next, so a harness that stopped
because it was waiting on an owner and one that stopped because it had finished
were indistinguishable after the fact — and neither carried the cost that
justified the choice.

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

Inference is what makes "every cycle persists a decision" an invariant rather
than an aspiration. The stated path stays open for a cycle that learns to name
its own reason; nothing fabricates one from prose today.

## Budget honesty

Elapsed time, retries and pending actions are measured at the funnel. Tokens,
cost and open loops are optional and report **absent, not zero** — the
rationale reads "tokens unmeasured" rather than implying a free cycle. Token
telemetry does not reach this boundary yet, and a `0` inside a budget
rationale would be a fabrication, not a measurement.

## What a decision does not grant

`last_cycle_decision` lands in `ProgramRuntimeState` through the existing
bookkeeping allowlist, and is deliberately **not** a control field. A cycle
narrating "I am waiting on approval" describes itself; it must not thereby
acquire the power to pause its own lane.

For the same reason a self-reported decision does not suppress future scheduled
cycles. The terminal rule means *this cycle ends and the harness takes no
further iteration on its own authority*. Permanently gating a cron lane on a
judgement the agent made about itself is how a harness wedges itself off;
pausing a lane stays the owner's act, through the pause index that already
drops a dispatch.

## Key files

- `harness/cycle_continuation.rs` — the enum, the budget snapshot, the
  derivation, and the durable record.
- `harness/program.rs` — the `last_cycle_decision` runtime-state field.
- `learning/program_state_bridge.rs` — bookkeeping allowlist entry, with the
  test pinning that it is not a control field.
- `api/web_api.rs` — the post-cycle funnel where each decision is settled.

The full boundary plan is [Bounded Research and Harness
Adaptation](../../archive/plans/2026-08-09-bounded-research-and-harness-adaptation-plan.md).
