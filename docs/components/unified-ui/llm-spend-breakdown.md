# LLM spend breakdown (`/llm` Spend section)

Design: 2026-07-25 LLM spend breakdown design (archived, implemented)

## What it is

A fixed, **account-wide** cost headline at the top of `/llm`'s default **Cost**
tab (the page is three tabs — Cost / Usage / Health): three cards — **Today**,
**Last 7 days**, **Last 30 days** — each showing the total `$` spend plus a
breakdown **by operation** and **by model** (compact labeled bars, top 6 with a
`+K more ($Y)` roll-up).

It is deliberately **independent of the page filter bar** (agent / operation /
model / range). The filter bar keeps driving every exploratory widget below; the
Spend section always answers "where did the money go" account-wide.

## Surface map

- **Section** — `<section id="spend">` in
  `src/routes/(app)/llm/+page.svelte`. Deep-linkable via `/llm#spend`
  (`scroll-margin-top` clears the sticky filter bar). Renders `spendViews`
  (derived), with per-window loading skeleton, empty (`$0.00 · no paid calls`),
  and per-window hard-error states.
- **Pure logic** — `src/lib/llm/spendBreakdown.ts` (unit-tested in
  `spendBreakdown.test.ts`):
  - `buildSpendBreakdownQueries(boundaries, anchorMs)` → the six
    `{window, dimension, sql, lane}` specs ({today, 7d, 30d} × {operation, model}).
- **Time bounds and lanes** — `src/lib/llm/queryWindow.ts` (unit-tested):
  rolling windows become literal epoch-ms lower bounds, and `today` gets the
  short-window batch lane.
  - `parseSpendRows`, `summarizeSpend`, `rollupSpendRows` — parsing + totals +
    top-N roll-up.
- **Day boundaries** — reuses `localDayBoundaries` from `$lib/today/pulseQueries`
  (never duplicate calendar-day math). "Today" is the LOCAL calendar day, inlined
  as epoch-ms literals so DuckDB's UTC clock can't skew the window (matches the
  Today's Pulse band and the `#today` section).

## Data flow

Each of the six queries is wired through
`setupLiveDataSource({ kind: 'llm_calls_sql', sql, lane })`. Same-kind,
same-lane sources auto-batch into one POST to
`/api/magician/v2/analytics/llm_calls/query_batch` (client cap
`LLM_CALLS_BATCH_MAX_QUERIES` = 16 in `useLiveDataSource.ts`, which must not
exceed the server constant of the same name in `magician-api/src/analytics_api.rs`).
Only one batch per lane is in flight, so lanes keep small windows fast:

- **Short-window lane** (`LLM_SHORT_WINDOW_LANE`): spend Today × 2, the
  `#today` comparison and the 24h spend timeline — plus the range-driven
  widgets when the range is 24h.
- **Default lane**: spend 7d/30d × 2, Decision Models, the KPI row, top agents
  and top operations.

On arrival the Cost tab therefore sends two concurrent batch requests. Its
reveal levels open at 0 ms so the staged widgets join those batches instead of
queueing behind them. That endpoint materializes a
**de-duplicated `llm_calls` view** (one row per call), so `SUM(cost_usd)` does
**not** double-count. Never query `read_parquet` directly.

Per-window SQL shape (operation shown; model uses
`COALESCE(NULLIF(model, ''), 'unknown')`):

```sql
SELECT COALESCE(NULLIF(operation, ''), 'unattributed') AS label,
       SUM(cost_usd) AS spend, COUNT(*) AS calls
FROM llm_calls
WHERE <window>
  AND (COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate')
GROUP BY COALESCE(NULLIF(operation, ''), 'unattributed')
HAVING SUM(cost_usd) > 0 ORDER BY spend DESC
```

**Never filter the dimension `IS NOT NULL`.** The window total is summed
client-side from these rows, so anything the `GROUP BY` drops disappears from
the headline `$`. Unattributed rows are **bucketed**, not filtered. The `model`
bucket also collapses empty strings to `'unknown'`, matching `model_key` in
`buildTodayVsYesterdaySql`.

- `<window>` today: `timestamp_ms >= {todayStartMs} AND timestamp_ms < {tomorrowStartMs}`.
- `<window>` 7d/30d: `timestamp_ms > {anchorMs - 7/30 × 86400000}` (rolling),
  where `anchorMs` is the browser clock rounded **up** to the minute. The server
  narrows the materialization and partition lookback only from a literal
  `timestamp_ms >= N` / `> N` bound, never from `now() - INTERVAL`. Rounding up
  keeps the lookback at 7/30 days; rounding at all keeps the SQL string stable
  within a minute, so rewirers keyed on it do not refetch.
- The attempt guard excludes `logical_chunk_summary` aggregates — governed
  roll-up rows that never invoked a provider, whose cost is already carried by
  the underlying chunk calls; counting them adds that cost twice. Zero-attempt
  `harness_aggregate` rows (a managed CLI's logical aggregate, cost estimated)
  stay included. The read service rejects a successful zero-attempt row of any
  other `response_kind` (`llm_analytics_read_service.rs`). This section, the
  `#today` comparison (`REAL_CALL_PREDICATE` in the page) and the Today pulse
  band (`pulseQueries.ts`) use the same guard, so all three reconcile.

The per-window **total** and **paid-call count** are summed client-side from the
returned rows (operation-sum and model-sum cover the same paid calls → identical
totals; model rows are a fallback if the operation query errors).

## Refresh / lifecycle

Controllers are (re)wired by `syncSpendBreakdown` only when the local day or
the minute anchor has moved (`spendWiredKey` guard); it runs on tab open and on
a refresh, never on a filter edit. A same-minute refresh re-runs the wired
controllers instead. Controllers are torn down in `onDestroy`.

- **Wiring is gated on the tab.** The reactive block requires
  `activeTab === 'cost'`, so the six queries do not run on another tab (markup
  gating alone would not stop a reactive statement).
- **Leaving Cost tears the controllers down**, which aborts anything in flight
  (`destroy()` calls `activeRequest.abort()`). `teardownTab` also resets
  `spendWiredKey` to `''`, because the wiring guard fires only on a *change*;
  otherwise the section would return permanently empty.
- **Refresh is per-tab and scoped.** The Cost tab's own Refresh re-runs the six
  controllers directly, or rewires them when the day or minute anchor moved. It
  deliberately does **not** bump `lastRefreshed` or dispatch
  `magician:dashboard-refresh`: both are page-wide (`lastRefreshed` is a tracked
  dependency of the calls/governed/today/spend blocks, and every controller
  listens for that event on `window`), so either would make a Cost refresh
  re-run the other tabs' queries. DashboardChrome's Refresh still refreshes the
  live panel.
