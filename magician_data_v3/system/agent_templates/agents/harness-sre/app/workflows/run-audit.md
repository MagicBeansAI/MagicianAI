Run one bounded harness-reliability audit as the `harness-sre` agent.

Start from the caller's `focus` statement, then apply the Harness SRE operating
rules you already carry: read the deterministic `### Harness Health` block (or
the freshest available scheduler/anomaly/backlog facts) before narrative
memory, pick at most one concrete stuck state, and either record that the
harness is healthy or take exactly one bounded repair action.

Finish by projecting one `reliability_audit` record:

- `summary`: one or two sentences naming what you checked and what you did.
  State the evidence you relied on (health block lines, anomaly signatures,
  backlog item ids, task ids).
- `outcome`: `healthy` when no stuck state needed action, `repaired` when you
  completed one bounded repair or program-state update, `escalated` when you
  had to notify the owner or delegate instead.
- `audited_at`: the current UTC time.

Do not create additional records, do not mutate harness state beyond the one
action your rules permit per cycle, and do not widen this run into a general
briefing. If the harness is healthy, say so plainly and stop.
