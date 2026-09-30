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
  - `buildSpendBreakdownQueries(boundaries)` → the six `{window, dimension, sql}`
    specs ({today, 7d, 30d} × {operation, model}).
  - `parseSpendRows`, `summarizeSpend`, `rollupSpendRows` — parsing + totals +
    top-N roll-up.
- **Day boundaries** — reuses `localDayBoundaries` from `$lib/today/pulseQueries`
  (never duplicate calendar-day math). "Today" is the LOCAL calendar day, inlined
  as epoch-ms literals so DuckDB's UTC clock can't skew the window (matches the
  Today's Pulse band and the `#today` section).

## Data flow

Each of the six queries is wired through
`setupLiveDataSource({ kind: 'llm_calls_sql', sql })`. Same-kind sources
auto-batch (cap 8 ≥ 6) into a single POST to
`/api/magician/v2/analytics/llm_calls/query_batch`. That endpoint materializes a
**de-duplicated `llm_calls` view** (one row per call), so `SUM(cost_usd)` does
**not** double-count. Never query `read_parquet` directly.

Per-window SQL shape (operation shown; model uses
`COALESCE(NULLIF(model, ''), 'unknown')`):

```sql
SELECT COALESCE(NULLIF(operation, ''), 'unattributed') AS label,
       SUM(cost_usd) AS spend, COUNT(*) AS calls
FROM llm_calls
WHERE <window> AND COALESCE(provider_attempt_count, 1) <> 0
GROUP BY COALESCE(NULLIF(operation, ''), 'unattributed')
HAVING SUM(cost_usd) > 0 ORDER BY spend DESC
```

**Never filter the dimension `IS NOT NULL`.** The window total is summed
client-side from these rows, so anything the `GROUP BY` drops disappears from
the headline `$`. Unattributed rows are **bucketed**, not filtered. The `model`
bucket also collapses empty strings to `'unknown'`, matching `model_key` in
`buildTodayVsYesterdaySql`.

- `<window>` today: `timestamp_ms >= {todayStartMs} AND timestamp_ms < {tomorrowStartMs}`.
- `<window>` 7d/30d: `timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7/30 DAYS)` (rolling).
- `COALESCE(provider_attempt_count, 1) <> 0` excludes `logical_chunk_summary`
  aggregates — governed roll-up rows that never invoked a provider
  (`llm_trace_activation.rs` rejects a successful zero-attempt row for anything
  else), whose cost is already carried by the underlying chunk calls. Counting
  them adds that cost twice. This section, the `#today` comparison, and the
  Today pulse band all apply the same guard, so all three reconcile.

The per-window **total** and **paid-call count** are summed client-side from the
returned rows (operation-sum and model-sum cover the same paid calls → identical
totals; model rows are a fallback if the operation query errors).

## Refresh / lifecycle

Controllers are (re)wired only when the local day changes (`spendWiredDay`
guard); the 7d/30d windows slide forward on their own because their SQL is
anchored on `now()`. Controllers are torn down in `onDestroy`.

- **Wiring is gated on the tab.** The reactive block requires
  `activeTab === 'cost'`, so the six queries do not run while another tab is
  selected. Markup gating alone would not have achieved this — the block is a
  reactive statement that cannot see what is rendered.
- **Leaving Cost tears the controllers down**, which aborts anything in flight
  (`destroy()` calls `activeRequest.abort()`). `teardownTab` also resets
  `spendWiredDay` to `0`, because the wiring guard fires only on a *change* —
  leaving the sentinel set would make the section return permanently empty.
- **Refresh is per-tab and scoped.** The Cost tab's own Refresh re-runs the six
  controllers directly, or rewires them when the local day has rolled over. It
  deliberately does **not** bump `lastRefreshed` or dispatch
  `magician:dashboard-refresh`: both are page-wide (`lastRefreshed` is a tracked
  dependency of the calls/governed/today/spend blocks, and every controller
  listens for that event on `window`), so either would make a Cost refresh
  re-run the other tabs' queries. DashboardChrome's Refresh still refreshes the
  live panel.
