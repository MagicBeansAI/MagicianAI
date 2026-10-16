# Crew Health Projection

Crew health is a scoped operational read model shared by the normal Crew page
and Town Square. It reports current overall readiness separately from rolling
seven-day operations and durable daily history. It is observational only: the
score does not trigger alerts, block execution, change autonomy, or mutate an
agent definition.

## API

```text
GET /api/magician/v2/agents/health
GET /api/magician/v2/agents/{agent_id}/health
```

Both endpoints require a workspace-bound bearer. The
bulk endpoint returns every non-system agent in that scope. The single-agent
endpoint reads the same cached projection and returns `404` when that scoped
agent does not exist.

The response schema is `crew_health.v1`; formula version `1` preserves the
original Town Square baseline:

- baseline: 70;
- rolling 7-day LLM success contribution: -25 through +20 around a 90 percent
  neutral success rate;
- activity recency: +10 within one hour, +5 within one day, and -15 after seven
  days;
- current state: working +5, needs attention -10, paused -5, offline -25;
- final score clamped to 5 through 100.

`overall` is the current composite. It is not a score for the current calendar
day. Its raw `inputs`, component `contributions`, observation window, coverage,
and formula version are returned with the score. Bands are a read projection:
`good >= 70`, `fair >= 40`, and `poor < 40`.

`rolling_7d` keeps operational metrics distinct from the composite:

- LLM calls, spend, success rate, and last call for the trailing seven days;
- average health across the available daily snapshots in that window;
- oldest-to-newest score delta;
- improving, stable, declining, or insufficient-history trend;
- actual snapshot-day count, so a partial window is never presented as seven
  complete days.

## Sources And Degradation

Runtime/task state comes from the same scoped agent and v3 task services used by
the normal Crew API. LLM reliability and cost come from one fixed server-owned
query over the scoped `analytics/llm_calls` Parquet partitions. Clients do not
submit SQL or calculate a second formula.

The endpoint remains available when either optional source fails. It returns a
partial availability status, source limitations, and reduced per-agent coverage
instead of failing the Crew or Town Square page. No calls in a valid seven-day
window is different from an unavailable analytics source.

## Durable History

History is stored under:

```text
<scope>/analytics/agent_health/history.json
```

The file contains at most one snapshot per agent per UTC day and retains 90
days. A same-day snapshot is refreshed no more than once per hour. Writes are
atomic and serialized through the shared service; reads are cached for 60
seconds to prevent the normal Crew and Town Square polls from duplicating
DuckDB scans.

There is deliberately no synthetic backfill. History starts when the projection
is first observed, and `sample_days` exposes how much of a rolling window is
actually present. Corrupt or incompatible history is not overwritten; the API
returns live overall health with durable history marked unavailable.

Future formulas may add task, delivery, intervention, or business-outcome
signals. Such a change requires a formula-version bump and migration policy;
unlike versions must not be silently treated as one continuous trend.
