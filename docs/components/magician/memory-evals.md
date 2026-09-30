# Memory Retrieval Evals

## Purpose

Memory retrieval evals are scoped checks against the same prompt renderer used
by agent loops. Each case asks, "given this goal/query, did the memory renderer
surface the durable facts this agent needs?"

The chat-context live lane also gates the resident local embedding path. Its
default warm wall budgets are 500 ms p50 and 800 ms p95 for the configured
PPLX 4B embedder. The report keeps provider, ranking, temperature, and
procedure stages separate so a cache or provider regression cannot hide behind
one aggregate latency number. The revision-bound hybrid result cache is **on
by default** in Magician YAML; `MAGICIAN_MEMORY_HYBRID_RESULT_CACHE=off` is
the kill switch. Cache-on qualification must prove byte-equivalent score maps and
selected prompt entries against the same-binary cache-off control; the 100×
label is reserved for the frozen-index exact-hit workload with no journal
writes.

The runner emits one `eval_case` row per case per retrieval backend into the
scoped `memory_events` Parquet lakehouse. The `/memory` dashboard reads those
rows for pass-rate trends, backend comparison, failing cases, rank
distribution, and failure drilldown.

## Suite Locations

Built-in suites live in:

- `data/magician_v2/memory_evals/*.json`

Scope-local override or extension suites live in:

- `magician_data_v3/scopes/<principal>/<workspace>/memory/evals/*.json`

For the default `anonymous/default` scope, built-ins are automatically loaded
unless a scoped suite uses the same `suite_id`. That lets a workspace override a
built-in suite without changing the repo seed.

System-wide extra suites can also be placed in:

- `magician_data_v3/system/memory_evals/*.json`

## Suite Format

Retrieval cases are the default:

```json
{
  "suite_id": "personal-assistant-regression",
  "enabled": true,
  "cases": [
    {
      "case_id": "sota-test-runner-api",
      "agent_id": "personal-assistant",
      "query": "How should I execute local browser SOTA tests?",
      "scopes": ["user", "agent"],
      "expected_substrings": ["window.testRunner", "getResults"],
      "min_selected_count": 1,
      "max_entries": 10,
      "max_chars": 6000
    }
  ]
}
```

Fields:

- `suite_id`: stable suite identifier. Defaults to the filename stem when
  omitted for file-backed suites.
- `enabled`: optional; defaults to `true`.
- `case_id`: stable case identifier.
- `kind`: optional; defaults to `retrieval`. Use `lifecycle` for capture /
  consolidation / render checks.
- `agent_id`: agent whose memory tier configuration should be used.
- `query`: retrieval query passed into the memory renderer.
- `goal_id`: optional goal key for `agent_goal` scope retrieval.
- `scopes`: any of `user`, `agent`, `agent_goal`. Defaults to all three.
- `expected_substrings`: case-insensitive required snippets in the rendered
  memory output.
- `forbidden_substrings` (alias `forbidden`): case-insensitive snippets that
  must **not** appear. Every other assertion is recall-shaped — "did the anchor
  surface?" — so a case that injects eight entries to reach its anchor scores
  identically to one that injects exactly the right entry. Use these for the
  other half: a superseded fact that must stay excluded, a stale value a newer
  memory replaced, or a known distractor that must not out-rank the answer. A
  case may assert only `forbidden_substrings`.
- `min_selected_count`: optional minimum rendered memory entry count.
- `max_entries`: optional; defaults to 8.
- `max_chars`: optional; defaults to 4000.

At least one of `expected_substrings`, `forbidden_substrings`, or
`min_selected_count` must be present.

`skip_if_absent` never applies to a forbidden-substring violation: the thing
that must not appear did appear, which is a failure in any scope, not a case
that is inapplicable to this one.

Lifecycle cases use the same prompt renderer but also inspect native episodes
and consolidation audit records:

```json
{
  "suite_id": "memory-lifecycle-smoke",
  "enabled": true,
  "cases": [
    {
      "case_id": "simple-data-analyst-terminal-capture-consolidation-render",
      "kind": "lifecycle",
      "agent_id": "simple-data-analyst",
      "query": "How should I follow up on a Metabase GMV analysis?",
      "scopes": ["agent"],
      "expected_episode_candidate_types": [
        "terminal_outcome",
        "final_output_excerpt"
      ],
      "min_episode_candidates": 1,
      "min_consolidation_records": 1,
      "expected_consolidation_targets": ["semantic"],
      "expected_substrings": ["Metabase"],
      "min_selected_count": 1
    }
  ]
}
```

Lifecycle-only fields:

- `expected_episode_candidate_types`: candidate type names that must exist in
  native V3 episodes for the agent.
- `min_episode_candidates`: minimum total structured episode candidates.
- `expected_consolidation_targets`: targets that must appear in
  `memory_consolidation_audit.jsonl`.
- `min_consolidation_records`: minimum consolidation audit row count.

A lifecycle case passes only when all requested capture, consolidation, and
render assertions pass. Its `retrieval_backend` is recorded as `lifecycle`.

## Authoring Guidance

Good eval cases test durable retrieval behavior, not exact prose:

- Prefer stable identifiers, APIs, workflow names, dashboard/card IDs, source
  domains, or domain-specific terms.
- **Prefer consolidator-schema tier FIELD NAMES** (`research_patterns`,
  `effective_diagnostic_queries`, `reliable_query_templates`,
  `data_source_locations`, …) as anchors: the renderer prints the field name
  with every entry, and field names survive consolidation rewrites that
  freely re-phrase prose; exact phrases rot once consolidation rewrites a
  memory. Both backends failing identically is the signature of stale
  expectations, not a retrieval regression.
- Avoid brittle whole sentences from generated memory files.
- Keep one behavioral expectation per case where possible; when a case needs
  two substrings, prefer anchors that live in the SAME memory entry so
  ranking can't split them across the injection budget.
- Make the query lexically close to the target entry's own vocabulary — both
  retrieval backends rank entries against the query, and an anchor that
  exists in the tier still fails if its entry doesn't rank into
  `max_entries`/`max_chars`.
- Use `min_selected_count` to catch empty retrieval even when the exact string
  can reasonably vary.
- Use higher `max_chars` for dense semantic memories and lower values for user
  preference smoke checks.

Per-agent regression suites should cover the agent's high-risk memory contract:

- `personal-assistant`: user preferences, local browser test workflows, and
  recurring automation conventions.
- `internal-system-analyst`: trace/log/publication debugging patterns.
- `simple-data-analyst`: Metabase discovery, native-SQL dependency pitfalls,
  reliable query templates, and artifact data-source locations.
- `web-researcher`: search-query phrasing patterns, search-tool settings,
  environment knowledge, and snippet caution.
- `meetings-memory-regression` (user scope): the per-meeting takeaway entries
  the meeting rails append to `user.research_findings`
  (`key = meeting:<thread-id>`, `source_type = meeting_capture`). **Ships
  `enabled: false`** because those entries only exist after a live meeting
  capture — flip `enabled` (or override with a scoped suite of the same
  `suite_id`) after the first captured meeting.
- `weg-ambient-memory-regression` (user scope): guards the WEG
  evidence→memory→retrieval path — that an approved ambient fact (provenance
  `evd:amb:*`, rationale "ambient browsing evidence") is actually retrieved +
  selected for an on-topic work query. **Ships `enabled: true`, scope-gated by
  `skip_if_absent`** (see below): on a scope that never accrued ambient data the
  cases report `skipped` (not `failed`), so the guard is safe to run everywhere
  and only *asserts* against scopes that hold the data.

### `skip_if_absent` — safe global regression guards

A retrieval case may set `"skip_if_absent": true`. When the capped retrieval
surfaces none of the case's `expected_substrings`, the runner does a broad
presence probe of the scope's tiers (high entry/char caps). If the substrings
are **absent from the scope entirely**, the case is reported `skipped` (counted
separately — never as a failure), so a suite can ship `enabled` globally without
false-failing fresh/empty scopes. If the substrings **are present** but the
normal capped retrieval missed them, the case still **fails** — that is a real
retrieval regression, which is the whole point of the guard. Skips keep the run
`healthy`; the status reason discloses the skip count.

## Running Evals

The background runner is disabled by default. Set
`MAGICIAN_MEMORY_EVAL_ENABLED=true` to run it at boot and repeat on
`MAGICIAN_MEMORY_EVAL_INTERVAL_SECS` (default: 6 hours). Startup delay is
controlled by `MAGICIAN_MEMORY_EVAL_STARTUP_DELAY_SECS` (default: 60 seconds).
Manual dashboard/API runs remain available without enabling the periodic runner.

Manual run for the current HTTP scope:

```bash
curl -X POST http://localhost:3002/api/magician/v2/analytics/memory_events/evals/run
```

The `/memory` dashboard exposes the same action as **Run evals now**.

## Mixed Memory-Temperature Live Eval

`make test-memory-temperature-live-eval` is the local, cost-bearing acceptance
gate for the temperature layer. It starts/validates the configured Ollama
generation and embedding daemons, reconciles the rebuildable derived memory
index for `MEMORY_INDEX_PRINCIPAL` / `MEMORY_INDEX_WORKSPACE`, then writes
`report.json` and a clickable
`report.html` under `$(MEMORY_TEMPERATURE_LIVE_OUTPUT_DIR)` (on the SSD coverage
root by default).

The evaluator deliberately combines three kinds of evidence:

- Built-in retrieval suites run against the active real scope through the
  production hybrid renderer. These reads disable audit/usage persistence and
  background overlay repair. Only anchors that still exist in that scope count
  toward fixture coverage, while recall counts only anchors in lanes the
  production renderer can actually auto-inject. Search-only
  `environment_knowledge` and disabled prompt lanes therefore remain visible as
  existing fixtures without masquerading as prompt-recall failures. Retired
  fixtures are skipped. The report retains only bounded counts and hashes
  rather than raw memory text.
- An isolated synthetic workspace tests a cold but relevant fact against a hot
  distractor, exclusion of superseded memory, typed-lane diversity, hybrid
  retrieval, and the invariant that read-only retrieval cannot heat memory.
- The configured `memory_temperature_utility_review` operation is durably
  enqueued and claimed through the production incremental maintenance path,
  then reviews adversarial useful/irrelevant/harmful fixtures through the real local model.
  The gate checks accepted utility-label sets, hot-projection creation, marker
  preservation, and reviewer latency. Its structured-output schema requires a
  complete result set, while candidate identity remains runtime-owned and is
  restored by input position if a local model duplicates an opaque key.

The main metrics are anchor recall, current fixture coverage, mean reciprocal
rank, hybrid-backend coverage, warm retrieval p50/p95, cold snapshot p95,
utility-label accuracy, projection coverage, and projection factuality. Cold
snapshot construction stays visible in the report, while the latency gate uses
warmed cases, matching the warmup methodology of the chat-context retrieval
eval. Thresholds are configurable through the `MEMORY_TEMPERATURE_LIVE_*` Make
variables.

Production hybrid retrieval applies the active agent's visibility universe
inside both LanceDB legs before their bounded top-K: user-scoped memory plus
Agent and AgentGoal memory owned by that agent. The resulting score map is
still computed once and shared across those three prompt lanes. Every score is
bound through the manifest source record to the exact indexed candidate
revision it ranked; a write that lands after scoring therefore loses the stale
boost at the renderer handoff while retaining direct lexical ranking. This
prevents other agents' otherwise valid memories from crowding an authorized,
relevant memory out of the candidate set without broadening what the caller can
see.
The derived-index manifest also persists an exact vector compatibility ID. It
combines the shared Ollama toolkit contract (provider, model, dimensions,
logical context, physical `num_batch` ceiling, and toolkit preprocessing
version) with the memory index's own split/merge input version. A context,
physical-batch, or preprocessing change therefore marks the index incompatible
and selects full replacement even when model and dimensions are unchanged. The
on-disk embedding-cache namespace uses the same ID, so a rebuild cannot silently
refill from vectors produced under the old contract; execution-only tuning such
as request batching, timeouts, parallelism, and residency does not cause
needless invalidation.
Automatic prompt retrieval also removes search-only environment-knowledge rows
before top-K selection; explicit `search_memory` retains the wider authorized
universe. A small deterministic query expansion adds retrieval vocabulary for
provenance and endpoint/location questions. Finally, prompt ranking combines
hybrid relevance with a bounded candidate-level lexical signal, preserving
exact URLs, identifiers, and evidence fields that reciprocal-rank fusion alone
can flatten. The same expanded query guides excerpt selection, so a relevant
fact near the end of a long memory remains visible after prompt budgeting.
Explicit provenance and endpoint/location intents receive a narrow reservation
inside their existing semantic lane: provenance candidates are ordered by
source structure plus subject overlap, while structured location candidates
must match at least two non-expansion subject terms. Selected endpoint entities
may carry equivalent aliases found in other authorized matching memories, and
source-backed entries receive a provenance label derived from their stored
evidence markers. These enrichments preserve source IDs and do not increase the
lane or global entry budget.

The reported **utility review p95** is background maintenance wall time, not
chat or task time-to-first-token. Completed runs enqueue their selected-memory
snapshot asynchronously; the periodic maintenance runner later asks the
configured local model to classify which memories were load-bearing, useful,
referenced, irrelevant, stale, or harmful and then updates temperature counters
and compact hot projections. A slow review therefore delays how quickly the
working set adapts and can create local CPU/GPU contention, but it is not awaited
by the response path.

The same coverage guards a prompt-packing edge case: a highly ranked memory
larger than its lane budget must not be dropped wholesale. Prompt rendering
emits Unicode-safe, query-focused bounded excerpts (including separated
relevant regions when useful), reserves a fair portion of each lane for every
already-selected item so the first oversized item cannot starve later results,
retains the source identity for audit/utility attribution, and still obeys both
lane and overall character budgets. A second excerpt window is allocated only
for a genuinely new query concept; repeated copies of one common word cannot
split the budget and clip the first, more specific identifier. Hybrid query relevance remains the dominant
ranking signal; temperature and compact hot-projection state are
working-set/representation priors and cannot outweigh a materially stronger
retrieval match.

The evaluator is also the final child in `make test live_evals=true` and
`make test-live-evals`. That ordering is intentional: it may make both the
embedding and generation models resident, so all other repository and live
tests finish first. Its HTML report is linked from
`coverage/evals/live-suite/latest.html` and then from the repository summary.
Use `LIVE_EVAL_ONLY=memory-temperature make test-live-evals` for the isolated
aggregate/reporting path. `make test-memory-temperature-eval-harness` is the
provider-free harness regression.

## ANN shadow eval

Default YAML is `runtime.retrieval.vector_search: ann`. Rollback is
`MAGICIAN_VECTOR_SEARCH=flat` or YAML `flat`.

`make test-memory-ann-shadow-eval-harness` is the provider-free merge gate
and a `/evals` harness lane
(`report=evals/memory-ann-shadow/deterministic/latest`). It covers mode
install and `MAGICIAN_VECTOR_SEARCH` override, flat `bypass_vector_index`
even when IVF exists from a prior mode (query-plan flags; small tables skip
IVF create below `min_rows`), shadow serving the same key order as flat,
ANN falling back to flat when IVF is missing, predicate application, and
recall@k as `|intersect(top-k)| / k`.

`make test-memory-ann-shadow-live-eval` is the dedicated `/evals` live
lane. It runs `make test-memory-temperature-live-eval` with
`MAGICIAN_VECTOR_SEARCH=ann_shadow` and writes
`evals/memory-ann-shadow/live/latest` so it cannot overwrite the
memory-temperature live report. Shadow still *serves* flat keys; ANN
compare is observe-only: it is spawned off the served hybrid return path
and capped at 50ms extra.

Keep `make test-memory-temperature-live-eval` and
`make test-chat-context-retrieval-live-eval` as the retrieval owner gates.
The owner-gated shadow live pass against a rebuilt index must still show:

1. required-anchor recall unchanged versus the same-binary `flat` control;
2. accepted top-K overlap (ordered mismatch is a recorded fact, not a silent
   score change — shadow still *serves* flat keys);
3. lower vector-leg wall time only as a secondary observation.

`MAGICIAN_VECTOR_SEARCH=flat` is the restart-bound rollback to exhaustive
KNN.

## Tier-Effectiveness Eval

Every eval above asks a *retrieval* question: given this query, did the renderer
surface the durable fact? None asks whether the temperature **tier** a memory
was placed in is correct or useful; tiering defects can survive while every
retrieval gate is green.

`make test-memory-tier-health-eval` covers that axis. It is provider-free and
read-only: it deserializes a scope's `temperature_overlay.json` and reports

| metric | question it answers |
| --- | --- |
| `unearned_active_ratio` | what share of T0+T1 has never been retrieved, selected, injected, used, or reviewed? |
| `working_set_ratio` | is the active tier a working set or most of the overlay? |
| `tier_lift` | P(selected \| T0/T1) ÷ P(selected \| T2/T3) — does the tier predict use at all? |
| `dead_entry_ratio` | how much of the overlay carries no signal of any kind? |
| `unmigrated_live_ratio` | legacy-encoded entries that still have a live candidate — real migration debt |
| `legacy_key_ratio`, `max_key_chars` | key hygiene (the first is reported, not gated — see below) |
| `singleton_partition_ratio` | lane fragmentation — how often capacity stops bounding anything |
| `superseded_active_count` | invariant: superseded memory must sit in T3 |

Point it at any overlay:

```bash
cargo run -p magician-vector-index --example memory_tier_health -- \
  --overlay <scope>/memory/index/temperature_overlay.json
```

Pass `--documents <documents.jsonl>` alongside `--overlay` to enable the
migration gate. Without it, `unmigrated live memory` reports `skipped`: a legacy
key on an entry whose candidate is gone is *evidence*, not debt, and the two are
indistinguishable from the overlay alone.

That distinction is why `legacy_key_ratio` is reported but never gated. It
cannot reach zero by correct behaviour — migration is candidate-driven, so an
entry that carries usage signal but whose candidate has been deleted or
consolidated away has nothing left to derive a new key from, and is kept under
its historical key permanently. Gating on it would demand a lossy re-parse of
the ambiguous old encoding — inventing data to satisfy a metric.
`unmigrated_live_ratio` asks the question that actually has a right answer: is
live memory on the current encoding?

Thresholds are `MemoryTierHealthGates` defaults, overridable per flag
(`--max-unearned-active-ratio`, `--min-tier-lift`,
`--max-unmigrated-live-ratio`, …). `tier_lift` and the migration gate report
`skipped` rather than passing or failing when their inputs are unavailable. The same metrics are folded into the memory-temperature live eval report
under `tier_health`, so the live suite gates on them too.

### Reading `tier_lift` against `unearned_active_ratio`

These two move independently and are most informative together. A high lift with
a high unearned ratio means the tier is a strong signal *for the memory
that earned its place*, while half the active tier is inert padding riding the
lane-capacity floor. Fixing the second raises the first.

## Candidate Key Encoding (schema v6)

Overlay entries and hot projections are keyed by candidate identity. Schema v6
length-prefixes every segment of `(scope, agent_id, goal_id, tier_name,
item_key)` (`crate::key_encoding`, same as the index change journal) behind an
`mt1:` prefix. Why: segments contain colons (`chat:<uuid>` goal ids, free-text
goals, `system:scheduler` agent ids), so a bare `:` join (≤ v5) was not
injective and lane partitioning recovered the wrong segments. Each segment is
bounded at 160 bytes — longer ones contribute a digest — so keys stay loggable
and storage-bounded; the full goal text lives on the candidate.

Migration is driven by candidates rather than by parsing old keys, because the
old encoding cannot be inverted:

- `sync_memory_temperature_overlay` renames lazily, per candidate, preserving
  every counter. It never evicts — its callers hold partial candidate sets.
- `resync_memory_temperature_overlay_full_scope` runs from the full-scope
  paths (a full index rebuild, and `POST /memory/temperature/maintain` with
  `resync_overlay`). Only it may compact.

Retention is bounded by `MemoryTemperatureRetentionPolicy`. Entries carrying
usage signal, supersession links, or a live candidate are never evicted by age;
everything else is dropped after the TTL, with a bounded record appended to
`memory/index/temperature_compaction_audit.json`.

## Automated Regression Enforcement

Magician treats memory evals as an in-runtime health gate rather than requiring
CI to enforce them. After every periodic or manual memory eval run, the runner
writes the latest scoped status snapshot to:

```text
magician_data_v3/scopes/<principal>/<workspace>/memory/eval_status/regression_status.json
```

The same run emits a `memory_regression_status` row into `memory_events` so the
dashboard and analytics queries can track status changes over time.

Status values:

- `healthy`: at least one eval case ran and all latest cases passed.
- `degraded`: all latest cases passed, but one or more indexed retrieval evals
  had to fall back to direct tier retrieval. This usually means the derived
  memory index is stale, missing, or unavailable.
- `failing`: one or more latest eval cases failed. The status snapshot includes
  bounded failure records with suite id, case id, agent id, backend, status, and
  payload excerpt.
- `unknown`: no enabled eval cases ran yet, or no status snapshot exists.

Read the latest status through:

```bash
curl http://localhost:3002/api/magician/v2/memory/regression/status
```

Internal agents can use `internal_data` action `memory_regression_status` for
the same scoped snapshot.

## Result Rows

Each case writes an `event_kind = 'eval_case'` row with:

- `eval_suite`, `eval_case_id`, `agent_id`, `scope`, `eval_query`
- `eval_pass`, `expected_count`, `matched_count`, `selected_count`, `best_rank`
- `retrieval_backend`: `direct`, `lancedb_hybrid`, `direct_fallback`, or
  `lifecycle`. `direct_fallback` means the LanceDB hybrid pass was requested
  but the derived index was stale, missing, or unavailable, so direct tier
  ranking produced the rendered memory.
- `selected_item_keys`: JSON array of rendered memory item keys
- `status`: `passed`, `failed`, `missing_agent`, `invalid_case`, or
  `render_error`
- `payload_json`: backend, selected keys, matched/missing substrings, bounded
  rendered excerpt, and for lifecycle cases the candidate/consolidation counts

Example query:

```sql
SELECT eval_suite, eval_case_id, agent_id, retrieval_backend,
       eval_pass, matched_count, expected_count, best_rank, status
FROM memory_events
WHERE event_kind = 'eval_case'
ORDER BY timestamp_ms DESC
LIMIT 50;
```

## Dashboard Panels

The memory observability dashboard shows regression status (current, history,
latest failure count), eval pass rate and case counts, direct-vs-LanceDB-hybrid
backend comparison (overall and by suite), pass/fail trend, pass rate by agent,
status breakdown, worst recurring cases, rank distribution, latest runs, failure
drilldown with rendered payload excerpts, consolidation quality (status,
skipped output, source kind, target, recent issues), and temperature overlay
state (lane/tier inventory, utility label counts, active hot projections,
inactive projection lifecycle reasons).

Panels are intentionally SQL-backed so new eval fields can be explored without
a migration.
