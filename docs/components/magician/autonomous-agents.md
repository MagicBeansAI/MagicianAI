# Autonomous Agents

Most agents act when spoken to. An agent with an `autonomous_config` block acts
on its own schedule, with no user turn to trigger it. That block's presence is
the enablement — there is no `enabled` flag.

## The declaration

From [`ambassador`](../../../magician_data_v3/system/agent_templates/agents/ambassador/definition.agent.yaml):

```yaml
autonomous_config:
  schedule: "8,38 * * * *"          # twice an hour
  focus_areas:
    - name: inbound sweep
      description: Read what arrived on the company's own mailbox and number
        since the last cycle. Answer what the charter already answers, escalate
        what needs the founder, and leave nothing silently unread.
      priority: high
      schedule: "8,38 * * * *"
      program: outward_charter.md
    - name: follow through
      priority: medium
      schedule: "50 */6 * * *"
```

Three things are worth noticing:

- **Focus areas carry their own schedule.** An agent can sweep its inbox every
  half hour while chasing follow-through every six.
- **A focus area names a `program`** — a markdown charter the cycle is run
  against, so the standing instruction lives in the store rather than in Rust.
- **Priority is declared**, not inferred, so a cycle that cannot do everything
  has an ordering.

## What ships

Nine shipped agents declare the block. **Seven fire:**

| Agent | Schedule |
| --- | --- |
| `ambassador` | `8,38 * * * *` |
| `cro` | `5,35 * * * *` |
| `cmo` | `10,40 * * * *` |
| `cpo` | `15,45 * * * *` |
| `cto` | `20,50 * * * *` |
| `ceo` | `25,55 * * * *` |
| `harness-sre` | `7 * * * *` |

Two more — `creative-mind` and `personal-assistant` — are declared but parked on
`0 0 31 2 *`. February 31st never occurs, so they never fire. That is a
deliberate off-switch that keeps the configuration intact.

The staggered five-minute offsets across the C-suite set are not incidental:
those agents are an ongoing experiment in whether an organisation's standing
work can be decomposed across scheduled roles. Treat that set as a bet, not a
shipped product.

## How a cycle differs from a chat turn

An autonomous cycle is a distinct code path, not a simulated user message.

- `pipeline/agent.rs` carries `is_autonomous_cycle` through the run.
- The `ExecutionRun` has no chat session id — `realtime_events.rs` documents
  `None` for autonomous origins, and `artifact_v2/models.rs` treats
  scheduler/API/autonomous origins as background-spawned work the user owns.
- `learning/work_ledger_program_state.rs` distils progress against
  `autonomous_config.focus_areas`, so a cycle's output is attributable to the
  focus area that produced it rather than to an anonymous background run.

Because the run is owned but unattended, everything in
[The OS Primitives](../../architecture/os-primitives.md) applies with more force:
an unattended agent is exactly the one that should be spending against a budget
it cannot exceed.

## Related

- [Recurring Monitors](recurring-monitors.md) — standing watches, the narrower cousin
- [Resurfacing](resurfacing.md) — what unattended work surfaces to Today
- [Agent templates](../../../magician_data_v3/system/agent_templates/README.md) — the full schema
