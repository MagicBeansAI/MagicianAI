# Memory Retrieval Evals

## Purpose

Memory retrieval evals are scoped checks against the prompt renderer the agent
loops use. Each case asks: given this goal or query, did the renderer surface
the durable facts this agent needs?

The runner writes one `eval_case` row per case per retrieval backend into the
scoped `memory_events` Parquet lakehouse. The `/memory` dashboard reads those
rows for pass-rate trends, backend comparison, failing cases, rank
distribution and failure drilldown.

The chat-context live lane also gates the resident local embedding path:
default warm wall budgets are 500 ms p50 and 800 ms p95 for the configured PPLX
4B embedder. The report keeps provider, ranking, temperature and procedure
stages apart so a cache or provider regression cannot hide in one latency
number. The revision-bound hybrid result cache is **on by default** in Magician
YAML; `MAGICIAN_MEMORY_HYBRID_RESULT_CACHE=off` is the kill switch. Cache-on
qualification must prove byte-equivalent score maps and selected prompt entries
against a same-binary cache-off control; the 100× label is reserved for the
frozen-index exact-hit workload with no journal writes.

## Suite Locations

| Location | Role |
| --- | --- |
| `data/magician_v2/memory_evals/<suite>.json` | Built-in suites, compiled into the binary (`include_str!`); loaded for the `anonymous/default` scope only. |
| `magician_data_v3/scopes/<principal>/<workspace>/memory/evals/*.json` | Scope-local overrides or extensions. |
| `magician_data_v3/system/memory_evals/*.json` | System-wide extra suites. |

A built-in is skipped when a scoped suite uses the same `suite_id`, so a
workspace can override a built-in without changing the repo seed. Other scopes
run only their scoped suites plus the system suites.

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

| Field | Meaning |
| --- | --- |
| `suite_id` | Stable suite id; defaults to the filename stem for file-backed suites. |
| `enabled` | Optional; default `true`. |
| `case_id` | Stable case id. |
| `kind` | Optional; default `retrieval`. `lifecycle` checks capture, consolidation and render. |
| `agent_id` | Agent whose memory tier configuration is used. |
| `query` | Retrieval query passed to the renderer. |
| `goal_id` | Optional goal key for `agent_goal` scope. |
| `scopes` | Any of `user`, `agent`, `agent_goal`; default all three. |
| `expected_substrings` | Case-insensitive snippets that must appear in the rendered memory. |
| `forbidden_substrings` (alias `forbidden`) | Case-insensitive snippets that must **not** appear: a superseded fact, a replaced value, a distractor that must not out-rank the answer. Every other assertion measures recall only, so this is the precision half. A case may assert only this. |
| `min_selected_count` | Optional minimum rendered entry count. |
| `max_entries` | Optional; default 8. |
| `max_chars` | Optional; default 4000. |

At least one of `expected_substrings`, `forbidden_substrings` or
`min_selected_count` is required. `skip_if_absent` never excuses a
forbidden-substring hit: that is a failure in any scope.

Lifecycle cases use the same renderer and also inspect native episodes and
consolidation audit records:

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

| Lifecycle-only field | Meaning |
| --- | --- |
| `expected_episode_candidate_types` | Candidate types that must exist in the agent's native V3 episodes. |
| `min_episode_candidates` | Minimum structured episode candidates. |
| `expected_consolidation_targets` | Targets that must appear in `memory_consolidation_audit.jsonl`. |
| `min_consolidation_records` | Minimum consolidation audit rows. |

A lifecycle case passes only when every capture, consolidation and render
assertion passes; its `retrieval_backend` is `lifecycle`.

## Authoring Guidance

Test durable retrieval behavior, not exact prose:

- Anchor on stable identifiers, APIs, workflow names, dashboard/card IDs,
  source domains or domain terms.
- **Prefer consolidator-schema tier field names** (`research_patterns`,
  `effective_diagnostic_queries`, `reliable_query_templates`,
  `data_source_locations`, …). The renderer prints the field name with every
  entry and it survives consolidation rewrites; exact phrases do not. Both
  backends failing the same way points to stale expectations, not a
  retrieval regression.
- Avoid whole sentences from generated memory files.
- Keep one expectation per case; when two substrings are needed, pick anchors
  from the same entry so ranking cannot split them across the budget.
- Phrase the query in the target entry's vocabulary: an anchor that exists in
  the tier still fails if its entry does not rank into
  `max_entries`/`max_chars`.
- Use `min_selected_count` to catch empty retrieval when exact strings vary.
- Use a higher `max_chars` for dense semantic memories, lower for preference
  smoke checks.

Per-agent regression suites cover each agent's high-risk memory:

- `personal-assistant`: user preferences, local browser test workflows,
  recurring automation conventions.
- `internal-system-analyst`: trace/log/publication debugging patterns.
- `simple-data-analyst`: Metabase discovery, native-SQL dependency pitfalls,
  reliable query templates, artifact data-source locations.
- `web-researcher`: search-query phrasing, search-tool settings, environment
  knowledge, snippet caution.
- `meetings-memory-regression` (user scope): per-meeting takeaways the meeting
  rails append to `user.research_findings` (`key = meeting:<thread-id>`,
  `source_type = meeting_capture`). **Ships `enabled: false`**, since the
  entries exist only after a live capture; enable it (or override with a scoped
  suite of the same `suite_id`) after the first captured meeting.
- `weg-ambient-memory-regression` (user scope): checks that an approved ambient
  WEG fact (provenance `evd:amb:*`, rationale "ambient browsing evidence") is
  retrieved and selected for an on-topic work query. **Ships `enabled: true`,
  gated by `skip_if_absent`**, so scopes without ambient data report
  `skipped`.

### `skip_if_absent` — safe global regression guards

With `"skip_if_absent": true`, when capped retrieval surfaces none of the
`expected_substrings`, the runner probes the scope's tiers with high caps. If
the substrings are **absent from the scope**, the case is `skipped` (counted
separately, never a failure), so a suite can ship enabled everywhere. If they
**are present** but capped retrieval missed them, the case **fails**: that is a
real regression. Skips keep the run `healthy`; the status reason gives the skip
count.

## Running Evals

The background runner is off by default. `MAGICIAN_MEMORY_EVAL_ENABLED=true`
runs it at boot and every `MAGICIAN_MEMORY_EVAL_INTERVAL_SECS` (default 6
hours), after `MAGICIAN_MEMORY_EVAL_STARTUP_DELAY_SECS` (default 60 seconds). Manual
runs for the current HTTP scope work without it:

```bash
curl -X POST http://localhost:3002/api/magician/v2/analytics/memory_events/evals/run
```

The `/memory` dashboard's **Run evals now** does the same.

## Mixed Memory-Temperature Live Eval

`make test-memory-temperature-live-eval` is the local, cost-bearing acceptance
gate for the temperature layer. It starts or validates the configured Ollama
generation and embedding daemons, reconciles the derived memory index for
`MEMORY_INDEX_PRINCIPAL` / `MEMORY_INDEX_WORKSPACE`, and writes `report.json`
and `report.html` under `$(MEMORY_TEMPERATURE_LIVE_OUTPUT_DIR)` (on the
coverage root by default). It combines three kinds of evidence:

- **Built-in suites on the real scope** through the production hybrid
  renderer, with audit/usage persistence and overlay repair off. Fixture
  coverage counts only anchors still in the scope; recall counts only anchors
  in auto-injectable lanes, so search-only `environment_knowledge` and disabled
  lanes do not count as recall failures. Retired fixtures are skipped. The
  report keeps counts and hashes, not memory text.
- **An isolated synthetic workspace**: a cold relevant fact against a hot
  distractor, superseded-memory exclusion, typed-lane diversity, hybrid
  retrieval, and the invariant that read-only retrieval cannot heat memory.
- **The `memory_temperature_utility_review` operation**, enqueued and claimed
  through the production maintenance path, reviewing adversarial
  useful/irrelevant/harmful fixtures with the real local model. It checks
  utility-label sets, hot-projection creation, marker preservation and reviewer
  latency. The output schema requires a complete result set; candidate identity
  stays runtime-owned and is restored by position if the model duplicates a
  key.

Metrics: anchor recall, fixture coverage, mean reciprocal rank, hybrid-backend
coverage, warm retrieval p50/p95, cold snapshot p95, utility-label accuracy,
projection coverage and factuality. The latency gate uses warmed cases (cold
snapshot time is reported, not gated), like the chat-context eval. Thresholds
are the `MEMORY_TEMPERATURE_LIVE_*` Make variables.

**Utility review p95** is background maintenance time, not chat time-to-first
token. Completed runs enqueue their selected-memory snapshot; the maintenance
runner later asks the local model which memories were load-bearing, useful,
referenced, irrelevant, stale or harmful, then updates temperature counters and
hot projections. A slow review delays adaptation and can contend for CPU/GPU,
but no response waits on it.

### Retrieval behavior the eval guards

- **Visibility before top-K.** Production hybrid retrieval applies the active
  agent's visibility universe (user memory plus that agent's Agent and
  AgentGoal memory) inside both LanceDB legs before their bounded top-K, so
  other agents' memories cannot crowd out an authorized one. One score map is
  shared across the three lanes.
- **Revision-bound scores.** Each score is bound through the manifest source
  record to the exact candidate revision it ranked; a write after scoring loses
  the stale boost at the renderer but keeps lexical ranking.
- **Vector compatibility ID.** The derived-index manifest stores an ID built
  from the shared Ollama toolkit contract (provider, model, dimensions, logical
  context, physical `num_batch` ceiling, preprocessing version) and the index's
  split/merge input version. A change in any of them forces full replacement,
  even with the same model and dimensions. The on-disk embedding cache uses the
  same ID, so a rebuild cannot refill from old vectors. Execution tuning
  (batching, timeouts, parallelism, residency) does not invalidate.
- **Prompt ranking.** Automatic retrieval drops search-only
  environment-knowledge rows before top-K; explicit `search_memory` keeps the
  wider authorized universe. A small deterministic query expansion helps
  provenance and endpoint/location questions. Ranking adds a bounded lexical
  signal to hybrid relevance so exact URLs, identifiers and evidence fields
  survive reciprocal-rank fusion. Provenance and endpoint/location intents get
  a narrow reservation within their lane (provenance candidates ordered by
  source structure plus subject overlap; location candidates must match at
  least two non-expansion subject terms); endpoints may carry aliases from
  other authorized memories, and source-backed entries get a provenance label
  from their stored evidence markers. These keep source IDs and do not raise
  lane or global entry budgets.
- **Oversized entries.** A top-ranked memory larger than its lane budget is
  excerpted, not dropped: Unicode-safe excerpts focused by the expanded query
  (so a fact near the end of a long memory stays visible), a fair share of
  each lane reserved for every selected item, source identity kept for
  attribution, lane and global budgets obeyed. A second excerpt window opens
  only for a new query concept. Query relevance stays dominant; temperature and
  hot projections are priors that cannot beat a much stronger match.

The evaluator runs as a child of `make test live_evals=true` and
`make test-live-evals` (`scripts/run-live-evals-with-report.sh`); its report is linked
from `coverage/evals/live-suite/latest.html`. Use
`LIVE_EVAL_ONLY=memory-temperature make test-live-evals` to run it alone.
`make test-memory-temperature-eval-harness` is the provider-free harness test.

## ANN shadow eval

Default YAML is `runtime.retrieval.vector_search: ann`. Rollback, restart-bound,
is `MAGICIAN_VECTOR_SEARCH=flat` or YAML `flat` (exhaustive KNN).

`make test-memory-ann-shadow-eval-harness` is the provider-free merge gate and
a `/evals` harness lane (`report=evals/memory-ann-shadow/deterministic/latest`).
It covers mode install and the `MAGICIAN_VECTOR_SEARCH` override, flat
`bypass_vector_index` even when IVF exists from a prior mode (small tables skip
IVF below `min_rows`), shadow serving the same key order as flat, ANN falling
back to flat when IVF is missing, predicate application, and recall@k as
`|intersect(top-k)| / k`.

`make test-memory-ann-shadow-live-eval` is the live `/evals` lane: it runs
`make test-memory-temperature-live-eval` with `MAGICIAN_VECTOR_SEARCH=ann_shadow`
and writes `evals/memory-ann-shadow/live/latest`, so it cannot overwrite the
memory-temperature live report. Shadow still *serves* flat
keys; the ANN comparison is observe-only, off the served return path, capped at
50ms extra.

`make test-memory-temperature-live-eval` and
`make test-chat-context-retrieval-live-eval` stay the retrieval owner gates. The
owner-gated shadow pass on a rebuilt index must show:

1. required-anchor recall unchanged versus the same-binary `flat` control;
2. accepted top-K overlap (an ordering mismatch is recorded, not a silent score
   change);
3. lower vector-leg wall time, as a secondary observation only.

## Tier-Effectiveness Eval

Retrieval evals do not ask whether a memory sits in the right temperature
**tier**, so tiering defects can pass every retrieval gate.
`make test-memory-tier-health-eval` covers that. It is provider-free and
read-only: it reads a scope's `temperature_overlay.json` and reports:

| metric | question it answers |
| --- | --- |
| `unearned_active_ratio` | Share of T0+T1 never retrieved, selected, injected, used or reviewed? |
| `working_set_ratio` | Is the active tier a working set or most of the overlay? |
| `tier_lift` | P(selected \| T0/T1) ÷ P(selected \| T2/T3): does tier predict use? |
| `dead_entry_ratio` | Share of the overlay with no signal at all. |
| `unmigrated_live_ratio` | Legacy-encoded entries that still have a live candidate (real migration debt). |
| `legacy_key_ratio`, `max_key_chars` | Key hygiene (the first is reported, not gated). |
| `singleton_partition_ratio` | Lane fragmentation: how often capacity bounds nothing. |
| `superseded_active_count` | Invariant: superseded memory must sit in T3. |

```bash
make test-memory-tier-health-eval \
  MEMORY_TIER_HEALTH_OVERLAY=<scope>/memory/index/temperature_overlay.json
```

The target runs the `memory_tier_health` example and passes
`--documents` (`MEMORY_TIER_HEALTH_DOCUMENTS`, default the scope's
`memory/index/documents.jsonl`), which enables the migration gate; without it
`unmigrated live memory` reports `skipped`, because a legacy key whose
candidate is gone is kept evidence, not debt. For the same reason
`legacy_key_ratio` is never gated: migration is candidate-driven, so such
entries keep their historical key forever, and gating would force a lossy
re-parse of the old encoding.

Thresholds are `MemoryTierHealthGates` defaults, overridable per flag through
`MEMORY_TIER_HEALTH_FLAGS`: `--max-unearned-active-ratio` (0.05),
`--max-working-set-ratio` (0.35), `--min-tier-lift` (1.5),
`--max-dead-entry-ratio` (0.50), `--max-unmigrated-live-ratio` (0.0),
`--max-key-chars` (1024). `tier_lift` and the migration gate report
`skipped` when inputs are missing. The memory-temperature live report includes
the same metrics under `tier_health` and gates on them.

### Reading `tier_lift` against `unearned_active_ratio`

Read them together. High lift with a high unearned ratio means the tier is a
strong signal for memories that earned their place, while much of the active
tier is inert padding on the lane-capacity floor. Fixing the second raises the
first.

## Candidate Key Encoding (schema v6)

Overlay entries and hot projections are keyed by candidate identity. Schema v6
length-prefixes each segment of `(scope, agent_id, goal_id, tier_name,
item_key)` (`crate::key_encoding`, shared with the index change journal) behind
an `mt1:` prefix. Segments can contain colons (`chat:<uuid>` goal ids,
free-text goals, `system:scheduler`), so the old bare `:` join (≤ v5) was not
injective. Each segment is capped at 160 bytes (longer ones become a digest);
the full goal text lives on the candidate.

The old encoding cannot be inverted, so migration is driven by candidates:

- `sync_memory_temperature_overlay` renames lazily per candidate, keeping every
  counter. It never evicts, since its callers hold partial candidate sets.
- `resync_memory_temperature_overlay_full_scope` runs on full-scope paths (full
  index rebuild, `POST /memory/temperature/maintain` with `resync_overlay`) and
  is the only one that may compact.

Retention follows `MemoryTemperatureRetentionPolicy`. Entries with usage
signal, supersession links or a live candidate are never evicted by age; the
rest drop after the TTL, with a bounded record in
`memory/index/temperature_compaction_audit.json`.

## Automated Regression Enforcement

Memory evals are an in-runtime health gate, not a CI gate. After every run the
runner writes the scoped status snapshot to:

```text
magician_data_v3/scopes/<principal>/<workspace>/memory/eval_status/regression_status.json
```

and emits a `memory_regression_status` row into `memory_events`.

| Status | Meaning |
| --- | --- |
| `healthy` | At least one case ran and all latest cases passed. |
| `degraded` | All passed, but some indexed retrieval evals fell back to direct tier retrieval (index stale, missing or unavailable). |
| `failing` | One or more cases failed; the snapshot has bounded failure records (suite, case, agent, backend, status, payload excerpt). |
| `unknown` | No enabled case has run, or no snapshot exists. |

```bash
curl http://localhost:3002/api/magician/v2/memory/regression/status
```

Internal agents get the same snapshot from `internal_data` action
`memory_regression_status`.

## Result Rows

Each case writes an `event_kind = 'eval_case'` row with:

- `eval_suite`, `eval_case_id`, `agent_id`, `scope`, `eval_query`
- `eval_pass`, `expected_count`, `matched_count`, `selected_count`, `best_rank`
- `retrieval_backend`: `direct`, `lancedb_hybrid`, `direct_fallback` (hybrid
  was requested but the index was stale, missing or unavailable) or
  `lifecycle`
- `selected_item_keys`: JSON array of rendered item keys
- `status`: `passed`, `failed`, `missing_agent`, `invalid_case` or
  `render_error`
- `payload_json`: backend, selected keys, matched/missing substrings, bounded
  excerpt, and lifecycle candidate/consolidation counts

```sql
SELECT eval_suite, eval_case_id, agent_id, retrieval_backend,
       eval_pass, matched_count, expected_count, best_rank, status
FROM memory_events
WHERE event_kind = 'eval_case'
ORDER BY timestamp_ms DESC
LIMIT 50;
```

## Dashboard Panels

The memory dashboard shows regression status (current, history, latest failure
count), pass rate and case counts, direct vs LanceDB-hybrid comparison
(overall and by suite), pass/fail trend, pass rate by agent, status breakdown,
worst recurring cases, rank distribution, latest runs, failure drilldown with
excerpts, consolidation quality (status, skipped output, source kind, target,
recent issues) and temperature overlay state (lane/tier inventory, utility
labels, active hot projections, inactive projection reasons). Panels are
SQL-backed, so new eval fields need no migration.
