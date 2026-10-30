# Memory Index

The memory index is a derived retrieval cache. Tiered memory JSON stays the
source of truth.

## Storage

Each scoped workspace keeps its index under
`magician_data_v3/scopes/<principal>/<workspace>/memory/index/`:

| File | Contents |
| --- | --- |
| `manifest.json` | Index/schema version, backend, rebuild time, source hashes, document and chunk counts, per-agent counts, embedding provider/model/dimension. |
| `documents.jsonl` | One `MemoryCandidateDocument` per line. |
| `lancedb/` | LanceDB table: BM25 FTS over chunk text plus an embedding column. Default ANN (`runtime.retrieval.vector_search: ann`): IVF_PQ shortlist plus exact L2 rerank. Flat KNN (`bypass_vector_index`) is the fallback and the `flat` mode. IVF_PQ is kept when `vector_search` is `ann_shadow` or `ann` and rows ≥ `ann.min_rows`. |
| `embedding_cache/` | Vectors scoped by provider/model/dimension, keyed by exact embedding input; rebuilds embed only new or changed chunks. |
| `changes.json` | Durable, generation-tagged journal of canonical writes not yet indexed; coalesces per user-memory source or native tier without losing a newer racing write. |

Every document keeps provenance: `scope`, `tier_name`, `goal_id`, `item_key`,
`source_path`, `json_pointer`, `content_hash`, `last_updated`, `confidence`.

LanceDB rows are chunks (`chunk_key`, parent `candidate_key`, `chunk_index`,
char offsets, metadata, `chunk_text`, `search_text`, vector) that aggregate to
their candidate before ranking. Candidates over 2,000 characters split into
2,000-character chunks with 200 characters of overlap; `task_progress.notes`
entries over 4,000 characters are first compacted into deterministic digests.

### Episodes

Episodes (`memory/agents/<id>/episodes/*.json`, not a declared tier) are
loaded by `collect_scope_memory_candidates` through
`magician_vector_index::episode_candidates` with `tier_name: "episodes"`,
`scope: Agent`, `goal_id` = `goal_key`, `item_key` = `episode_id`, and an
empty `json_pointer` (the `forget_memory` whole-file marker).

`POST /api/magician/v2/memory/search` and the `search_memory` tool share one
recall; the Memory page uses it to show scope, tier and semantic type and
records no temperature use. Its agent selector defaults to Personal Assistant
when available; displayed milliseconds cover the full request, not Lance alone.

- **Score oracle only.** Chat turns load episodes from disk; the index supplies
  the hybrid score. Unindexed episodes rank by keyword.
- **One builder.** `episode_candidate_document` over
  `EpisodeCandidateProjectionV1` serves both paths, since the score key hashes
  the composed text's `content_hash` (pinned by
  `the_disk_and_runtime_projections_of_an_episode_agree` and
  `both_paths_produce_the_same_index_score_key`). An unparseable file is
  skipped, costing one episode.
- **Incremental writes.** `append_native_episode` and deletion record
  `MemoryIndexChange::Episodes { agent_id }` (re-reads that agent's episode
  directory; invalidates by agent plus tier name, ignoring goal), so
  `search_memory` never returns deleted text. An agent missing from the
  definitions takes the full-rebuild path. A failed journal write still marks
  the scope dirty (next dirty rebuild, 15–30 minutes).
- `outcome_summary_preview` mirrors `truncate_text_for_audit` (whitespace
  collapsed to one line, elision marked), not `truncate_projection_text`.
- **Integrity.** As in `validate_loaded_native_episode`, `agent_id` must match
  the directory and `goal_key` be non-empty; the directory alone is not proof
  of ownership (a restored or copied file can name another agent).

## End-To-End Architecture

Canonical state, request-time state and search acceleration are separate, so
a slow or damaged index cannot change the answer:

1. **Canonical commit.** Memory flows write scoped user, agent or agent-goal
   tier JSON via the atomic-file and advisory-lock path. Only these files are
   authoritative.
2. **Mutation journal.** The writer then records a generation-tagged,
   source-addressable entry in `index/changes.json` (the correctness
   boundary) and sends a best-effort in-process notification (latency only).
3. **Prompt snapshot refresh.** Notifications coalesce for 100 ms and rebuild
   warm snapshots; every request also checks file length and mtime stamps,
   and a build retries if a file changes under it.
4. **Index maintenance.** The maintainer snapshots the journal, reloads
   affected sources, updates documents, chunk rows, embedding cache,
   temperature overlay and manifest, and acknowledges only generations it
   applied; a racing newer generation stays pending.
5. **Retrieval.** The query is embedded and LanceDB hybrid scores used when
   manifest and table are compatible. A pending source-addressable entry drops
   only that source's scores (its rows rank lexically; other scores stay
   usable); structural deltas or an incompatible or unreadable index select
   direct ranking. Then temperature and hot
   projections, ranking, exact dedup, lane selection and budget packing.
   Near-duplicate claim comparison prepares tokens once and shortlists by
   shared words within each semantic tier, preserving first-match order and
   the existing qualifier, number and strongest-representative rules.
   Explicit recall caches source-validated candidates without building prompt-only
   comparison profiles and applies engagement containment before ranking.
   Goal records share a snapshot per tier rather than one cache entry per goal.
   Inspection reuses current temperature snapshots; recorded retrieval still
   follows the durable temperature update path.
   Ordinary memories skip app-registry
   eligibility work; app-sourced memories still require live source validation.
   Revision-bound passage hits are indexed once per batch, preserving scores,
   chunk tie-breaking and canonical text hydration.
   Search traces separate embedding, admission, table checkout, FTS and the
   vector leg (including native exact reranking); the Lance timeout excludes embedding.

   Request-path caches (on by default; kill switches in
   [Embedding Provider](#embedding-provider)):
   - **Hybrid result cache.** Keyed by storage root, visibility predicate,
     embedding contract, index generation (monotonic by `rebuilt_at`),
     pending-change identity, scoring contract and ranking epoch (bumped by
     IVF_PQ creation or ANN parameter changes). Vector settings are captured
     once per request; ANN has its own scoring contract. A hit skips journal,
     inspect, embedding and Lance. Failures, timeouts, fallbacks (direct or
     ANN-to-KNN, served but not kept), stale overlays and cancelled leaders
     are never kept; a hard-stale inspect drops entries, a transient Lance
     timeout does not.
   - **Query-vector LRU.** Exact contract-plus-query key; a hit still runs
     Lance. Queued same-contract waiters share one `/api/embed` with the
     tightest deadline; a lone miss never waits
     (`embedding_query_batch_window_ms: 3`, `max_items: 8`).
   - **Vector leg.** ANN shortlists `limit * candidate_multiplier`; Lance reranks
     those original vectors by exact L2 in the same query, with stable key tie-breaking.
     Missing IVF (not a stale reason), errors or slowness inside
     the configured Lance timeout fall back to flat or empty vector keys, still
     fused with FTS. `MAGICIAN_VECTOR_SEARCH=flat` forces KNN; `ann_shadow`
     serves flat and records recall@k off the serve path (probe ≤50 ms).
     Cache keys include `candidate_multiplier` and `nprobes`. Identical
     concurrent queries singleflight even before a generation is published;
     unpublished loads are not kept.
   - **Table pool.** Vector and FTS handles share one generation, so RRF never
     fuses mixed snapshots; cancel, timeout or error discards them, and every write, optimize,
     quarantine or hard-stale inspect invalidates the pool before any
     directory replace.
6. **Result and feedback.** The bounded block enters context; telemetry and
   usage go to an ordered background worker. Signals may change temperature
   or hot projections, never a canonical fact.

- **Reindex/rebuild** reconstructs derived state from all sources. Only the
  explicit endpoint or CLI may atomically replace a missing or incompatible
  `lancedb/`; runtime reconciliation is merge-only and otherwise ranks
  directly.
- **Optimize/compact** runs LanceDB `optimize` (compaction, version pruning,
  index folding). Every incremental merge adds a fragment and search cost grows
  with the active count (480 fragments: 771 ms p50 against 192 ms once
  compacted to 2, measured 2026-10-28). The maintainer therefore compacts a
  healthy scope whose active fragment count exceeds
  `MAGICIAN_MEMORY_INDEX_OPTIMIZE_FRAGMENTS` (default 64; `0` disables),
  checking each scope at most every 10 minutes and logging
  `compacted fragmented memory index`. It runs the same
  `optimize_scope_memory_index` as `memory-index optimize`, under the same
  locks; the command remains for manual runs.

A canonical commit is visible at once. Snapshots and indexes may lag, but a
pending delta removes its old scores before a result returns, and retained
scores are keyed by the manifest's exact source revision, so a write after
the second journal snapshot cannot inherit an old score.

## Prompt Read Snapshots

Prompt requests without a recency cutoff use a bounded, process-local
immutable snapshot of canonical candidates with precomputed lexical features,
built once per source generation; output is unchanged. Key: storage root,
agent, scope, goal, environment-memory policy, tier-definition hash. Per-key
build locks prevent stampedes; readers may briefly hold an old generation.
Retrieval deadlines do not cancel an in-flight cache build; CPU profiling uses
an admitted blocking worker. Memory-index events refresh changed warm entries
under the same per-key lock without resetting their idle expiry.

Stamp changes are checked against memoized BLAKE3 content hashes, so touches
and identical rewrites retain the snapshot. Real changes reuse unchanged
episode/tier documents, lexical profiles and pairwise overlap links; user files
are re-read. The overlap graph falls back to a full build when more than 64
texts, or a quarter of the set, are new. Build logs report source loading,
profiling and overlap-graph timings and reuse counts.
Concurrent renders share one temperature tier map per overlay `Arc` for up to
30 seconds (`PROMPT_TIER_SNAPSHOT_REUSE`).

Configure under `memory.prompt_snapshot` in `magician-config.yaml`:

| Property | Default | Notes |
| --- | --- | --- |
| `enabled` | `true` | False builds per request. |
| `max_bytes` | `67108864` | 64 MiB estimate. |
| `max_entries` | `8` | Distinct scope/agent/goal snapshots. |
| `idle_ttl_secs` | `600` | Idle eviction. The live and seed configs set `86400`, so the first turn after a break does not rebuild. |
| `refresh_debounce_ms` | `100` | In-process refresh coalescing. |

## Rebuild And Status APIs

Scope comes from the workspace-bound Magician bearer.

```text
GET  /api/magician/v2/memory/index/status
POST /api/magician/v2/memory/index/rebuild
```

`status` compares candidate hashes with the manifest:

| Condition | Severity | Effect |
| --- | --- | --- |
| Missing, corrupt or incompatible index | hard-stale | Direct extraction until `rebuild` succeeds. |
| `source_hashes_changed` | hard-stale | Old vectors could mis-rank changed content. |
| `pending_changes` (full status) | hard-stale | So `--skip-if-fresh` cannot strand a journal entry. |
| `document_count_changed` | soft-stale | Indexed candidates rank; new ones score directly. |
| Missing table, index or schema entry; failed health probe | hard-stale, repairable | |
| LanceDB health-check timeout | transient | Direct ranking; maintainer skips repair until it responds. |

Hashes cover stable identity and content fields, not timestamps. Full
inspection checks the schema, the `chunk_key` and `candidate_key` BTree
indexes and the `search_text` FTS index;
`GET /api/magician/v2/memory/index/status` is lightweight (manifest plus file
health).

`rebuild` (explicit) walks agent definitions plus persisted user, agent and
agent-goal memory, and runs `merge_insert` on `chunk_key` against a
current-schema table: update on `row_hash` change, insert new, delete missing.
It replaces LanceDB via an atomically promoted temporary sibling when the
table is missing or incompatible, or the previous manifest is missing,
unreadable, an older index/schema/backend version, or from another embedding
provider/model/dimension. It acknowledges exactly the journal generations it
snapshotted, after every artifact commits.

The runtime maintainer uses `reconcile_scope_memory_index` (merge only); its
sole bootstrap is a first table (empty for an empty scope) when neither
manifest nor table exists. Anything else incompatible, corrupt or failing to
merge needs an explicit rebuild; runtime never renames or removes `lancedb/`.

`save_user_knowledge` and `save_native_tier` commit JSON, then journal; the
maintainer later merges only that source's rows (via the `candidate_key`
BTree) and acknowledges after documents, rows and manifest update. The
user-memory set is the four legacy files plus each distinct non-legacy
`scope: user` tier in active definitions (shared names once, with a
deterministic schema owner). Journal operations take a cross-process advisory lock
(five seconds max); a failed journal write falls back to the legacy dirty
notification. Startup consumes pending entries. Definition changes,
forget/delete, unsupported shapes and failed deltas trigger a whole-source
audit, applied row by row. The periodic scan only audits out-of-band edits
that cannot journal.

Offline operator path (same layout and code); `rebuild` also takes
`--skip-if-fresh` and `--force`:

```bash
./magician.bin memory-index status --principal anonymous --workspace default
./magician.bin memory-index rebuild --principal anonymous --workspace default
./magician.bin memory-index rebuild --principal anonymous --workspace default --skip-if-fresh --force
./magician.bin memory-index optimize --principal anonymous --workspace default
make memory-index-status
make memory-index-prewarm
make memory-index-rebuild
make memory-index-optimize
```

`make memory-index-prewarm` runs `rebuild --skip-if-fresh` and may replace
incompatible state. `make run-supervisor` skips it unless
`MAGICIAN_MEMORY_INDEX_PREWARM=1`. `--force` rebuilds even a fresh or
soft-stale index that `--skip-if-fresh` would skip (for one prewarm:
`MEMORY_INDEX_REBUILD_FLAGS=--force`). Targets default to
`anonymous/default`; override with `MEMORY_INDEX_PRINCIPAL=<principal>` and
`MEMORY_INDEX_WORKSPACE=<workspace>`.

For a private-copy comparison, set `MAGICIAN_LANCE_BENCH_DIR` to a copied
Lance directory and `MAGICIAN_LANCE_BENCH_VECTOR` to its query-vector JSON, then
run `make cargo ARGS="test -p magician-vector-index --lib vector_search_private_snapshot_benchmark -- --ignored --nocapture"`.
The probe compares legacy/native results and performs scalar maintenance; use a disposable copy.

## Embedding Provider

Local Ollama embeddings; default model
`hf.co/mykor/pplx-embed-v1-4b-GGUF:Q6_K`. The contract is configured only
under `runtime.ollama` in `magician-config.yaml`:

| Property | Notes |
| --- | --- |
| `embedding_base_url` | Embedding-only endpoint, default `http://127.0.0.1:11435`. |
| `embedding_keep_alive` | Must be `-1`; startup prewarms and pins. |
| `embedding_num_parallel` | Must be `1` (single-sequence runner); foreground first. |
| `embedding_max_loaded_models` | Must be `1`. |
| `embedding_query_timeout_ms` | Query budget incl. admission. Default `5000`. |
| `embedding_query_batch_window_ms` | Gather window only when a same-contract waiter is queued. Default `3`, cap `50`. |
| `embedding_query_batch_max_items` | Queries per `/api/embed`. Default `8`; `1` = pass-through. |
| `embedding_query_batch_max_chars` | Chars per query batch. Default `6000`. |
| `embedding_write_timeout_ms` | Per background batch. Default `180000`. |
| `embedding_model` | Recorded in manifest; change makes the index stale. |
| `embedding_dimensions` | Expected width; mismatch fails. |
| `embedding_context_tokens` | Ollama `num_ctx`. |
| `embedding_batch_tokens` | Ollama `num_batch` and per-input ceiling; change invalidates vectors. |
| `embedding_batch_size` | Starting texts per request. See [Batch width](#batch-width). |

### Batch width

Width starts at `embedding_batch_size`; a retriable Ollama failure halves it
and four consecutive clean batches double it, up to the configured value (the
counter resets at each step and on any failure, so a flapping endpoint
settles). Progress lines
report `active_batch_size` and `configured_batch_size`. One width serves
`EmbeddingPriority::{Read, BackgroundRead, Write}` but binds only on
multi-input calls; query micro-batches are capped by
`embedding_query_batch_max_items`.

Only `embedding_model`, `embedding_dimensions`, `embedding_context_tokens`,
`embedding_batch_tokens` and the preprocessing version form the manifest's
embedding contract; `embedding_base_url` is execution-only (unit-tested), so
moving the daemon never stales an index. Protected app-memory partitions carry
the URL in `MemoryEmbeddingPhysicalIdentity`.

Emergency env overrides:

| Variable | Default | Notes |
| --- | --- | --- |
| `MAGICIAN_MEMORY_EMBEDDING_PROVIDER` | `auto` | `auto` or `ollama`; non-empty rebuilds need real embeddings, else fail and rank directly. `test_hash` needs its test-only feature. |
| `MAGICIAN_MEMORY_OLLAMA_URL` | `runtime.ollama.embedding_base_url` (`http://127.0.0.1:11435`) | Endpoint override; generation URL is last fallback. |
| `MAGICIAN_OLLAMA_EMBEDDING_TIMEOUT_MS`, `MAGICIAN_MEMORY_EMBEDDING_TIMEOUT_MS` | `runtime.ollama.embedding_write_timeout_ms` (`180000`) | Per write batch; writes do not share the generation daemon or queue. |
| `MAGICIAN_OLLAMA_EMBEDDING_WARMUP_TIMEOUT_MS`, `MAGICIAN_MEMORY_EMBEDDING_WARMUP_TIMEOUT_MS` | `300000` or rebuild timeout, whichever is larger | One best-effort warmup before a non-empty rebuild; failure is logged and batches still run. |
| `MAGICIAN_MEMORY_QUERY_EMBEDDING_TIMEOUT_MS` | `runtime.ollama.embedding_query_timeout_ms` (`5000`) | Timeout selects direct ranking. |
| `MAGICIAN_MEMORY_EMBEDDING_MAX_BATCH_CHARS` | `6000` | Per rebuild request; one large chunk may exceed it. |
| `MAGICIAN_MEMORY_EMBEDDING_RETRIES` | `2` | Single-item timeout/429/5xx retries; not queries. |
| `MAGICIAN_MEMORY_EMBEDDING_RETRY_BACKOFF_MS` | `1500` | Linear backoff base. |
| `MAGICIAN_MEMORY_EMBEDDING_WARMUP` | `true` | `false` skips rebuild warmup; queries never warm up. |
| `MAGICIAN_MEMORY_LANCEDB_RETRIEVAL_TIMEOUT_MS` | `runtime.retrieval.lance_query_timeout_ms` (`3000`) | FTS/vector budget after the query vector; timeout ranks directly and briefly suspends hybrid. |
| `MAGICIAN_MEMORY_LANCEDB_HEALTH_CHECK_TIMEOUT_MS` | `5000` | Timeout is transient, not corruption. |
| `MAGICIAN_MEMORY_LANCEDB_OPTIMIZE_TIMEOUT_MS` | `60000` | `memory-index optimize`. |
| `MAGICIAN_MEMORY_INDEX_WRITE_LOCK_TIMEOUT_MS` | `120000` | Cross-process lock for rebuild, quarantine, replace, merge, optimize. |
| `MAGICIAN_MEMORY_HYBRID_RESULT_CACHE` | `runtime.retrieval.result_cache.enabled` (`true`) | Restart-bound. `on`/`enabled`/`1`/`true` or `off`/`pass_through`/`0`/`false` (kills even when YAML is true). |
| `MAGICIAN_LANCE_TABLE_POOL` | `runtime.retrieval.lance_table_pool.enabled` (`true`) | Restart-bound. `off`/`pass_through` disables reuse (a cold hybrid opens two tables, a warm one none). |
| `MAGICIAN_QUERY_VECTOR_CACHE` | `runtime.retrieval.query_vector_cache.enabled` (`true`) | Restart-bound. `off` disables the LRU. |
| `MAGICIAN_VECTOR_SEARCH` | `runtime.retrieval.vector_search` (`ann`) | Restart-bound. `flat` / `ann_shadow` / `ann`; invalid ignored. |
| `MAGICIAN_EMBEDDING_QUERY_BATCH` | `runtime.ollama.embedding_query_batch_*` (window `3`, max items `8`) | Restart-bound. `off` = pass-through. |
| `MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS` | unset | Test-only `1` with `MAGICIAN_MEMORY_EMBEDDING_PROVIDER=test_hash`, where `magician-vector-index` has `test-hash-embeddings`; runtime builds lack it. |

Guarantees:

- The manifest records `embedding_provider: "ollama"` with the exact
  `runtime.ollama.embedding_model` and `runtime.ollama.embedding_dimensions`
  (required; no compiled fallback); queries use the same fields. Production
  never writes fake vectors; a failed embedding leaves the index missing or
  stale, records a rebuild failure and keeps direct ranking.
- Query embedding precedes the LanceDB deadline; a slow Ollama means direct
  ranking, never a corrupt mark. LanceDB reads abort, not detach, on cancel or
  timeout.
- Admission is process-wide and foreground-first; a running request is not
  cancelled, but background work releases admission after each small batch.
  Capacity above one fails closed.
- Each physical input is capped at min(`embedding_context_tokens`,
  `embedding_batch_tokens`) minus an 8-token reserve, applied as a UTF-8 byte
  cap (504 bytes with the seed's 512); longer text splits into semantic fragments whose
  validated vectors (count, dimension, finite) are length-weighted and
  normalized into one. `truncate: false` is always set; the contract and
  cache namespace include both limits.
- Identical concurrent query embeddings coalesce across memory and procedure
  retrieval, reused for 25 ms (128 entries) under keys holding the full
  contract; failures are not reused.
- Document vectors cache under
  `memory/index/embedding_cache/ollama-<model>-<dimensions>/`, keyed by exact
  `search_text`.

## Continuous Maintenance

`MemoryIndexMaintainer` runs in the Magician service on the `magician-bg`
tokio runtime (started after the supervisor loops; earlier dirty marks are
caught by the first sweep). It coalesces dirty notifications by
`(principal, workspace)`, waits for the debounce (capped at 15–30 minutes from
the first dirty mark; cooldown still wins) and applies the journal delta.
Structural or unknown entries trigger an audit plus merge-only reconcile. A
periodic scan runs `inspect_scope_memory_index` and reconciles compatible
stale indexes (new, changed or pruned tier JSON, manual edits, tier-schema
definition changes, embedding model/provider changes).

Prompt-time hybrid retrieval (`score_lancedb_index` /
`score_lancedb_hybrid_index`) waits on a process semaphore and never fails on
it; maintenance does not use it. Lance offloads to its `lance-cpu` runtime, so
there is no `spawn_blocking` (tune with `LANCE_CPU_THREADS`,
`LANCE_IO_CORE_RESERVATION`).

| Variable | Default | Notes |
| --- | --- | --- |
| `MAGICIAN_MEMORY_INDEX_MAINTAINER` | enabled | `0`, `false`, `off` or `disabled` turns it off. |
| `MAGICIAN_LANCE_SEARCH_CONCURRENCY` | `runtime.scale` `lance_search_cost_units` | Restart-bound request-path permits; overrides `configure_lance_search_concurrency`. Profile value: 4 for `current`/`m2_max`, 1 for `small_cpu`, 8 for `cloud_heavy`; `auto` picks `small_cpu` at ≤4 CPUs, else `current`. |
| `MAGICIAN_MEMORY_INDEX_STARTUP_DELAY_SECS` | `15` | Before first scan. |
| `MAGICIAN_MEMORY_INDEX_INTERVAL_SECS` | `120` | Scan interval. |
| `MAGICIAN_MEMORY_INDEX_DEBOUNCE_SECS` | `15` | Quiet time before rebuild. |
| `MAGICIAN_MEMORY_INDEX_OPTIMIZE_FRAGMENTS` | `64` | Compact a healthy index above this many active fragments; `0` disables. |

- **Empty scope.** Cooldown or suspension clears only after full inspection
  proves zero candidates; bootstrap then writes a stable zero-document
  manifest and acknowledges the exact pending generation. The maintainer skips the eval principals
  `live-eval` and `storage-live-eval`.
- **Failure cooldown.** A failed refresh writes
  `memory/index/rebuild-cooldown.json` (failure count, next retry, bounded
  error): 15 minutes, 30 minutes, then one hour, surviving restarts; expiry
  allows one half-open retry, success deletes it. Meanwhile hybrid retrieval
  is suspended for the scope; writes and ingestion continue. Startup or
  periodic-audit failures also enqueue the scope for dirty retry.
- **Rebuild required.** A reconcile that fails only because the manifest
  shows `index_version_changed`, `schema_version_changed`, `backend_changed`,
  `embedding_config_changed` or `missing_manifest` (with table data) pauses the
  scope instead: `memory/index/rebuild-required.json` records the reason and
  versions, the cooldown is cleared (no failure count, no hourly full
  inspection), and one `service_health` Attention notice per scope gives
  `./magician.bin memory-index rebuild --principal <p> --workspace <w>`. Each
  pass re-reads only the manifest; when a rebuild replaces it the pause lifts
  and the notice resolves (the rebuild API does this at once). Other failures
  keep the backoff; runtime still never replaces `lancedb/`.
- **Stability profile.** Generation on `:11434` (`*local_generation_model`
  pin), embeddings on `:11435` (`keep_alive: -1`, one sequence). The
  commented stability overrides in `magician_data_v3/.env.example` disable
  batch retries (`MAGICIAN_MEMORY_EMBEDDING_RETRIES=0`) and warmup and set
  `MAGICIAN_MEMORY_INDEX_INTERVAL_SECS=21600`,
  `MAGICIAN_MEMORY_INDEX_DEBOUNCE_SECS=15`; longer merge delays also delay
  result-cache reuse while changes remain pending. Query timeouts and 429/5xx
  suspend hybrid for five minutes; 401/404/model-not-found do not.
- **Definition hashes** come from YAML bytes, avoiding false
  `source_hashes_changed` from map ordering.
- **Removal** is source-driven: prune canonical JSON and `merge_insert`
  deletes missing rows (including the last) without replacing the directory;
  stale cache files are harmless.
- **Repairable read errors** (e.g. `failed to fill whole buffer`, retrieval
  timeout) suspend hybrid for the scope and request merge-only reconcile;
  corruption that blocks open or merge needs an operator rebuild. Quarantine
  and rebuild are explicit; optimize is explicit or fragment-triggered (above).
  All run under one mutex and cross-process lock.
  Reconciliation, under the same locks, keeps at most
  `MAX_LANCEDB_QUARANTINE_DIRS` (3) `lancedb.corrupt-*` directories, newest
  first; pruning is best-effort and never fails the reconcile.
- **Row reuse.** Rows matching `chunk_key`, `row_hash` and
  `embedding_input_hash` are reused (`embedded_rows`, `reused_rows` in the
  report). Runtime merges maintain scalar indexes once their unindexed tail
  reaches 5% of rows (bounded to 128–1024), merging delta indexes at eight parts.
  This never compacts, prunes or re-embeds; IVF maintenance stays explicit to
  avoid changing ANN recall. Smaller tails remain searchable.

Events: `memory_index_reconcile_started`, `memory_index_reconcile_completed`,
`memory_index_reconcile_failed` (with `provider`, `model`, `dims`,
`doc_count`, `duration_ms`, `stale_reason`, dirty reasons and the full
`error` chain); `memory_index_lancedb_rows_updated`,
`memory_index_lancedb_source_delta_updated`, and (explicit actions only)
`memory_index_lancedb_replaced` and `memory_index_lancedb_optimize_*`. With
`MAGICIAN_MEMORY_EMBEDDING_PROVIDER=auto` and Ollama down, non-empty
reconciles fail and direct-ranked prompts emit `memory_retrieval_fallback`
(human logs: one warning per fallback episode, five-minute summaries with
suppressed counts, a recovery message on the next hybrid success).

Logging: `"memory index maintainer completed"` is DEBUG when idle
(`scopes_rebuilt=0 scopes_failed=0`), else WARN. `init_tracing` pins `lance`
and `lance_index` to WARN to hide
`type=load_scalar_part index_type=inverted part_id=N`; override with
`--log-level lance=debug,...`.

## Retrieval Contract

Direct candidate extraction stays the source of truth. With a fresh or
soft-stale manifest, the FTS/BM25 and vector legs run as two concurrent
explicit-projection queries (each with its own `_score` / `_distance`
column, which LanceDB's native hybrid cannot project), fused by in-house RRF
matching LanceDB's `RRFReranker` (`k`, 0-based ranks). Fused score leads,
with stable tier/confidence tie-breakers. Missing, hard-stale or unreadable
indexes use direct lexical ranking. Production never uses deterministic
embeddings.

Telemetry: fallbacks and stale skips emit `memory_retrieval_fallback`; prompt
block rows put `retrieval_backend`, counts, size and fallback reason in
`payload_json`. Per-candidate `score` is relative relevance, not a
probability (direct fallback: tier priority, confidence, exact phrase/token
and fuzzy n-gram overlap); `selected = true` rows were injected, `selected = false` dropped
by budget. Eval rows report the backend actually used (`direct_fallback`, not
`lancedb_hybrid`, after fallback) plus `eval_pass`, `matched_count`,
`best_rank`.

`memory_prompt_blocks` exposes `score_hybrid_index_for_prompt` (embed,
lookup, fallback) and `render_memory_tiers_for_prompt_with_scores_result`;
`render_memory_tiers_for_prompt_with_index_result` composes them
(byte-identical for single-tier callers), and
`inject_memory_tiers_for_direct_execution` scores one query for the user,
agent and agent_goal tiers. The procedure index shares only query-embedding
coalescing.

Run-start retrieval seeds from the agent definition's tier list and logs
`Run-start memory seed completed` with `tier_count` and per-scope rendered
length (agent-goal is zero without `goal_id`). `tier_count` 0 means no
declared tiers; a zero length with tiers means nothing rendered. It runs inside the
autonomous-task deadline and `bootstrap_total_ms` warns above one second.

## Temperature And Projection State

Temperature is an overlay; canonical files are never mutated for it.

| File | Holds |
| --- | --- |
| `memory/index/temperature_overlay.json` | Per candidate key: semantic lane, temperature tier, usage/outcome counters, utility-review labels, last-use times. Schema v6 adds a bounded ledger of applied review runs so redelivery cannot double-count. |
| `memory/index/utility_review_queue.json` | Pending review evidence, retry/dead-letter state; expired leases are claimable after restart. |
| `memory/index/hot_projections.json` | Compact prompt-ready projections from the utility reviewer, with source refs, tier/item metadata, policy version, regeneration time and count. |

- Rendering validates overlay entries against the canonical snapshot (a
  30-second tier cache keyed by the overlay `Arc`); missing or mismatched
  or old-schema entries take the lane's safe default, with background repair
  at most every 30 seconds per scope. Canonical lifecycle metadata filters
  superseded candidates regardless of the overlay.
- `load → modify → save` is serialized by `overlay_write_lock`, taken only at
  leaf mutators (`sync_*`, `record_*_usage`, `record_*_utility_review`,
  `record_*_supersessions`, `maintain_*`). Wait diagnostics go to
  `magician::metrics::memory_temperature`, tagged `sync`, `prompt_usage`,
  `outcome_usage`, `utility_review`, `supersessions`, `maintenance`,
  `full_scope_resync`, `save` or `counter_flush`.
  Each cycle clones, mutates and serializes (straight to pretty JSON) on the
  blocking pool and skips the write when nothing changed.
- **Counter write-behind.** Usage-counter bookkeeping (`record_*_prompt_usage`,
  `record_*_retrieval_usage`, `record_*_outcome_usage`) is applied to the
  cached snapshot under the lock, so readers in the process see it at once,
  and is written by one overlay write per
  `MEMORY_TEMPERATURE_COUNTER_FLUSH_DELAY` (2 s), on the blocking pool.
  **Crash-loss window:** a process that dies without its shutdown flush loses
  up to 2 s of retrieved/selected/injected/outcome counts and their
  `last_*_at` times; nothing else. Sync, utility review (its run ledger),
  supersessions, maintenance, full-scope resync (and its compaction audit) and
  `save_*` still write through before returning, and each carries the pending
  counters exactly once and settles them. A failed flush is logged and keeps
  the updates, retried by timer with backoff (2 s → 60 s, 6 attempts), then by
  the next counter update, durable write or shutdown. The replay log (4,096
  updates per overlay) is used only when another process rewrites the file
  under pending updates: the next locked cycle replays them onto the new file.
  Graceful shutdown calls `flush_pending_memory_temperature_writes()` (bounded
  10 s) after producers quiesce; a storage without
  `MemoryStorage::shared_handle` writes counters through as before.
- `render_memory_section` enforces each lane's `max_entries` and `max_chars`.
  The utility reviewer `warn!`s when `operation_llm_router` is absent.
- Hot projections inject while active and source-hash-matched, keeping the
  source key as feedback identity; maintenance deactivates them (with a
  reason) on hash mismatch, stale verification, policy drift or age without
  injection. T0 is reserved for user-preference, procedure, project-context
  and entity lanes; episode projections compact into T1; raw evidence
  hydrates from canonical memory.

Lane budgets (missing lanes keep defaults; both caps `0` disable a lane;
reload applies to later renders):

```yaml
memory:
  prompt_lane_budgets:
    user:
      user_preference: { max_entries: 6, max_chars: 2400 }
    agent:
      procedure: { max_entries: 2, max_chars: 880 }
    agent_goal:
      project_context: { max_entries: 4, max_chars: 2520 }
```

**Episode quality.** Consolidation classifies episode-source prompts with
`memory_episode_quality_classification` (local Ollama memory-quality profile)
when the router is available, else a deterministic signal. Reviews cache by
batch content hash (successes survive restart; failures get a capped
exponential retry guard) and are singleflight per scope; batches stay
pending under foreground, full-lane or same-provider dispatch pressure.

**Owner memory** writes go through the shared
[memory lifecycle](memory-lifecycle.md): revisions are preserved,
`memory_lifecycle_review` decides cross-key/tier relationships before
guarded apply, pending/retired revisions are not recalled, ambiguous claims
get durable owner clarification, and observations cannot silently retire a
stated goal.

**Conflict review.** Agent and goal operational-tier merges use
`memory_conflict_review` (`memory_conflict_review_high_risk` for legacy
global targets) in target-level batches for pairs above the similarity
threshold or sharing a durable key. Normal review is mini-first with a
full-model retry; high-risk targets stay on the stronger profile. Plans are
built before tier/user file locks; the locked path only applies them. A
reviewed `replace_existing` keeps the old item as
`memory_lifecycle: superseded` (successor, reason, timestamp) and appends the
new one; a missing or failed review keeps both, and `UpsertBySimilarity`
never replaces without one. Reviews emit `memory_conflict_review` rows.
An identical copy is never reviewed: same durable key from the same field
(exact under key normalization, never by similarity) and every other field
equal except provenance and review/supersession bookkeeping. A differing
`type`, `status`, value or other claim field keeps the pair for the reviewer.
The copy folds into the active item it repeats instead of being appended (or
is dropped when a reviewed replacement retired that item in the same merge): larger `evidence_count`/`confidence` (not the sum: the distiller
re-reads its own tier), earliest `created_at`, `source_episodes`/`merged_from`
unioned by episode id.

**Consolidation normalization.** Supersession suffixes are stripped from
active insights (audit fields such as `before` exempt). Wrappers like
`distilled_insights` reduce to the declared collection; legacy `{key, value}`
maps (`org_state`, `product_state`, `gtm_state`) become sorted entries;
ambiguous objects fail closed. Existing rich items under a `{key, value}`
contract take `name`/`title`/`surface`/`summary` as key (newest duplicate
wins; existing durable state only, under the tier lock); fresh output is
strictly validated with one schema-repair attempt.

**Contradiction sweep.** A same-key sweep over operational tiers first folds
identical copies into the earliest one with no model call and no cap, marking
each copy superseded (`memory_contradiction_sweep_identical_duplicate`, with a
`superseded_id` unique per fold so byte-identical copies keep distinct
candidate keys). It
then reviews at most 48 remaining pairs (batches of 12), supersedes only on
explicit replacement, and stamps kept-both pairs and engine abstentions
(answered below the gate: `memory_contradiction_sweep_unresolved`) with the
decision-policy key, so neither is re-sent until the engine revision or
reference version changes. A transient failure (timeout, admission refusal,
failed item, unavailable engine) stamps nothing and is retried next sweep. Superseded entries are audit-only
(T3, score `0.0`), skipped by prompts and `search_memory`; `/memory` shows
them. `POST /api/magician/v2/memory/temperature/maintain` triggers the same
path; its user-memory sweep runs lifecycle reconciliation. A snapshot mismatch or unavailable reviewer keeps both items.

Verification slices: `docs/runbooks/2026-06-14-fractal-memory-temperature-tiers.md`.

### Overlay Retention And Eviction

- `first_seen_at` is the retention anchor, stamped once by
  `apply_memory_temperature_overlay_sync` and never moved. `None` counts as
  older than any TTL, so true orphans stay collectable; key migration keeps
  counters but stamps no anchor.
- Neutral outcomes count: `apply_memory_temperature_outcome_usage` stamps
  `last_used_at`, and `has_any_temperature_signal` counts it.
- Hot projections migrate with the overlay on every path (API handler and
  index rebuild) via `migrate_memory_hot_projection_keys_for_scope`;
  migrating the overlay alone strands them.
- `load_memory_temperature_overlay_snapshot` and
  `load_memory_hot_projection_index_snapshot` cache only when the file stamp
  is unchanged across the read; `memory_temperature_scope_partition` returns a
  `Cow` prefix slice.

### Supersession Key Namespace

`MemoryTemperatureEntry.superseded_by` must hold a temperature candidate key
(`memory_supersession_chains` resolves it with `entries.get(key)`), as
`apply_memory_temperature_supersessions` stores.
`mark_memory_item_superseded_with_reason` stamps `superseded_by_item_key` =
`item_memory_key(incoming_item)` (nothing is written without a
`key`/`name`/`id`-style field; the reader falls back to legacy). `superseded_by_for_candidate` prefers it, composing the sibling key
with `memory_temperature_candidate_key_from_parts`, then
`superseded_by_candidate_key`, then legacy `superseded_by` /
`superseded_by_key`. **Known bound:** keys change on supersession, so chains
resolve one hop, then `successor_missing_from_overlay`.

## Code Knowledge

`SemanticMemoryType::CodeKnowledge` matches tier names `code_knowledge` /
`codebase` / `source_code` / `architectural` before the `"knowledge"`
keyword (`infer_semantic_memory_type`, `magician-vector-index`). Lane policy:
small T0, default T1, score 0.62, stale after 60 days, hot-projection
eligible, after Project Context; `MemoryPromptLaneBudgets::for_scope` gives
AgentGoal a share and Agent a smaller one (config key `code_knowledge`).

Coding workers (`kind: worker`), not default agents, declare
`codebase_knowledge` or `architectural_knowledge` (`{key, value, last_seen}`
plus `project_id`). The five coding agents (senior-software-developer,
principal-software-engineer, frontend-engineer, junior-frontend-engineer,
junior-software-engineer) distill via `consolidate_to_codebase_knowledge` or
`consolidate_to_architectural_knowledge` with `code_distill_system_v1.0.1` /
`code_distill_user_v1.0.0` (`data/magician_v2/prompts/`). Agents without
`run_coding_task` are out of scope.

**Write.** Output is a `[{key, value}]` array (`value_for_tier_root_merge`),
deduped by `key` through `upsert_by_name` / `durable_memory_key`. An
`EpisodeProjectResolver` (VibeDev: `VibeDev project: <uuid>` in the task)
stamps `project_id` before `apply_target` when the group agrees;
`execute_rule` splits multi-project batches
(`partition_code_batch_by_project`), and mixed unresolved batches stay
global; non-code rules, no resolver, single-project or non-`Episodes`
sources run once. `execute_rule_single` runs `redact_obvious_secrets` on code-knowledge
facts (`sk-`/`rk-`/`pk-`, `AKIA`/`ASIA`, `gh*_`, `xox*-`, JWTs,
`Bearer <token>`, `user:pass@`).

**Read.** Citizen tool `magician_code_knowledge { query, k? }` →
`POST /vibedev/citizen/code_knowledge` (`citizen_code_knowledge_handler`,
`magician-api/src/vibedev_api.rs`). The run-scope `CitizenGrant` supplies
`grant.agent_id` (the executing engineer, set at mint in `run_coding_task`); only legacy grants fall back to `grant.root_task_id` →
`get_task` → `manifest.agent_id` (the root task's `engineering-manager` has no
CodeKnowledge tier). `rank_memory_candidates_hybrid` (`tier_filter = None`)
results are filtered to the CodeKnowledge lane by
`candidate.semantic_memory_type`, minus superseded entries; usage is recorded
and top-`k` (default 6, clamped 1–20) return as
`{ok, facts:[{key, text, tier, score, source, temperature_tier}], backend}`.
Facts pass when `metadata_json["project_id"]` is empty (global) or equals the
run's project. `assets/pi-extensions/magician-citizen.ts` is `include_str!`'d,
so shipping it needs a magician rebuild. One read targets the run's single
agent.

New facts index via `save_native_tier` → journal → `MemoryIndexMaintainer`
(15-second debounce by default, including the recommended stability overrides).

## Internal Data Observability

Diagnostics tools:

- `memory_observability_summary {hours: 24}`: event/status/backend counts,
  selected-vs-dropped scores, low-score selections, fallbacks, failures.
- `memory_index_snapshot {}`: manifest, sidecar lines, LanceDB files,
  provider/model/dimensions, stale and failure diagnostics.
- `query_memory_events`: bounded `SELECT`/`WITH` over the `memory_events`
  view on `analytics/memory_events/dt=*/*.parquet`.

```sql
SELECT timestamp_ms, event_kind, status, retrieval_backend, payload_json
FROM memory_events
WHERE event_kind IN (
  'memory_retrieval_fallback',
  'memory_index_reconcile_failed',
  'memory_index_rebuild_failed'
)
ORDER BY timestamp_ms DESC
LIMIT 50;
```

A new retrieval backend must read derived data only when the manifest is
fresh, otherwise extract directly, and rebuild asynchronously.

Manifest backend `lancedb-hybrid-v2`, index version `memory-derived-index-v1`,
schema version 11 (chunk rows, collision-safe candidate keys, merge metadata,
`candidate_key` BTree, contract-bound vectors, revision-bearing source
hashes; v11 also neutralizes prompt-boundary tags in non-conversation
episodes); `documents.jsonl` is an auditable `jsonl-direct` sidecar. Any
older schema needs one explicit `memory-index rebuild` (or
`POST /api/magician/v2/memory/index/rebuild`) before accepting source deltas;
until then retrieval stays direct (see *Rebuild required* below).
