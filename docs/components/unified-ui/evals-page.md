# `/evals` — the eval suite as one surface

This page discovers the repository's eval lanes and shows readiness, previous
outcomes, duration, reports and available cost evidence.

Backend: [magician / eval lanes](../magician/eval-lanes.md) — read that for the
`## eval:` annotation format and its placement rules, which is where adding a
lane actually goes wrong.

Design: 2026-07-26 evals page design ·
plan: 2026-07-26 evals page plan

## Surface map

| file | owns |
| --- | --- |
| `src/routes/(app)/evals/+page.svelte` | the four tabs and every rendered state |
| `src/lib/evals/api.ts` | the wire client + normalisation at the boundary |
| `src/lib/evals/format.ts` | every presentation rule, pure and DOM-free |
| `src/lib/evals/format.test.ts` | unit tests over those rules |

Reachable from the command palette (`⌘K` → "Evals") and at `/evals`.

- **Overview** — every lane: readiness, last status, duration, cost, Run;
  client-paged over the single `/lanes` response.
- **Lane** — one lane's history plus a duration/status trend strip (24 runs,
  oldest → newest) and a link to the lane's report.
- **Cost** — spend by lane over a date range.
- **Runs** — chronological feed, filterable by lane, server-paged.

## Memory lifecycle

The `test-memory-lifecycle-live-eval` lane offers a partition, 1–3 repeats and
optionally up to three configured profiles, then **Run**. Choices load from the
Settings routing catalog (a new profile needs no Evals change); an empty
selection uses configured operation routes. Comparisons use private config
snapshots and leave live settings unchanged. Overview Run uses the current
controls, or defaults.

History links each run's own HTML report (failures and raw evidence). Model usage
and router cost estimates live in the report; the Cost tab shows unknown because
those calls run outside its task ledger. The deterministic `test-memory-lifecycle`
lane also has Run and report history. Comparison semantics and coverage limits:
[lifecycle contract](../magician/memory-lifecycle.md#focused-qualification).

## The two rules this page exists to enforce

### 1. An unknown cost renders `—`, never `$0.00`

`{kind:'unknown'}` (the ledger did not answer) and `{kind:'known', usd:0}` (the
run genuinely spent nothing) are different facts. Enforced in three places, and
**never** by a `?? 0` in the page:

- `normalizeCost` at the wire boundary — every malformed shape (missing or
  non-finite `usd`, unknown `kind`) degrades to *unknown*. The server serialises
  unknown as `{"kind":"unknown"}` with no numeric field at all.
- `formatCost` — `$0.00` only for a genuine zero; a spend that rounds to zero
  renders `<$0.01`.
- `formatAggregateCost` for totals.

### The aggregate form: a partial total is a floor

`aggregateCost` sums known costs and keeps `unknownCount`:

| state | renders |
| --- | --- |
| every cost known | the total, e.g. `$1.20` |
| some known, some not | **`≥$1.20`** — a floor, not a fact |
| nothing known | `—` |
| no runs at all | `$0.00` — no runs is no spend |

Mirrors the server's `SpendTotal::is_floor()` (`unknown_runs > 0` ⇒ `known_usd`
is a lower bound). The Cost scan is bounded (`COST_SCAN_CAP` = 1,000 runs); when
the range holds more, `costTruncated` says so on the page.

### 2. Nothing silently disappears

- A malformed annotation still draws its lane, with `parse_error` inline.
- An annotation bound to **no target** is a banner at the top — a missing eval is
  indistinguishable from one never written.
- An unrecognised run status is shown verbatim with the neutral tone (not a pass).
- An unrecognised `requires=` token passes through to the label unchanged.

## Wire contract — the shape that is easy to get wrong

`LaneView` **flattens** `EvalLane` and puts readiness beside those fields:

```json
{ "id": "eval-monitor-golden", "kind": "harness", "requires": [],
  "ready": true, "missing": [], "runnable": true, "last_run": null }
```

`ready` / `missing` / `runnable` are **top-level**; `listEvalLanes` lifts them
into a `readiness` object. Reading a nested object the server never sends fails
silently: every lane looks "not ready" with no service named and Run disabled.
Likewise `last_run` is a `RunView` — run fields flattened plus `cost`, no nested
`run`.

## Run gating

`runDisabledReason(lane)`, in this order:

1. **`kind === 'unknown'`** — nobody declared what the lane is, so it may be
   cost-bearing; outranks readiness.
2. **`parse_error` set** — the declaration cannot be trusted. Mirrors the
   server's `runnable = parse_error.is_none() && kind.runnable().is_some()`
   (`422`).
3. **not ready** — names *every* missing service at once.

`startRun` re-checks before the request so a stale grid cannot spend money, and
the server refuses independently (`409`); the 409 body (in-flight task or down
services) is shown **verbatim**.

## Notes

- **Authorization is not set here**: `installScopedApiFetch` attaches the
  workspace bearer to every `/api/magician` request and strips legacy scope
  selectors; hand-added principal/workspace would be a second source of truth.
- **`total` is the filter count**, a valid pager denominator; `listEvalRuns`
  floors it at the rows actually held.
- Every list request carries a monotonically increasing request id; stale
  responses are dropped.
- Presentation rules belong in `format.ts` (pure, tested), not the template.
