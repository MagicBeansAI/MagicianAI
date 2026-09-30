# Memory Index

The memory index is a derived cache for retrieval experiments. It does not
replace tiered memory JSON as the source of truth.

## Storage

For each scoped workspace, the index lives under:

```text
magician_data_v3/scopes/<principal>/<workspace>/memory/index/
```

The current derived backend writes:

- `manifest.json`: index version, schema version, backend name, rebuild time,
  source hashes, canonical document count, derived chunk count, per-agent
  counts, embedding provider, model, and embedding dimension.
- `documents.jsonl`: one serialized `MemoryCandidateDocument` per line.
- `lancedb/`: a local LanceDB table with a BM25 FTS index over composite chunk
  text plus an embedding column. Magician default is ANN
  (`runtime.retrieval.vector_search: ann`): IVF_PQ shortlist plus exact L2
  rerank. Exhaustive flat KNN (`bypass_vector_index`) is the fallback and the
  `flat` mode. IVF_PQ is maintained when `vector_search` is `ann_shadow` or
  `ann` and the table has at least `ann.min_rows` rows.
- `embedding_cache/`: provider/model/dimension-scoped binary vector cache keyed
  by the exact embedding input text. Rebuilds embed only new or changed chunks.
- `changes.json`: durable, generation-tagged journal of canonical memory writes
  waiting to be reflected in the derived index. Entries coalesce per user-memory
  source or native tier without losing a newer write that arrives while an older
  snapshot is being applied.

Every document keeps provenance back to the canonical memory source: `scope`,
`tier_name`, `goal_id`, `item_key`, `source_path`, `json_pointer`,
`content_hash`, `last_updated`, `confidence`.

### Episodes

Episodes are indexed alongside tier candidates, but they do not come from the
tier walk. They live one directory across
(`memory/agents/<id>/episodes/*.json`, one file per episode) and are not a
declared tier, so `collect_scope_memory_candidates` loads them separately
through `magician_vector_index::episode_candidates`. They carry
`tier_name: "episodes"`, `scope: Agent`, `goal_id` = the episode's `goal_key`,
`item_key` = its `episode_id`, and an empty `json_pointer`, which the
`forget_memory` commit path reads as the whole-file marker.

`POST /api/magician/v2/memory/search` and the `search_memory` tool share one recall. The Memory page uses it to show scope, tier, and semantic type for a query, and does not record a temperature use.

The index is a score oracle, not a retrieval surface, so indexing episodes does
not change how they are loaded: a chat turn still assembles episode candidates
from disk, and the indexed row only supplies the hybrid score those candidates
are ranked by. An episode with no indexed row (new since the last rebuild, or
unparseable) falls back to keyword ranking.

One builder produces the candidate for both paths
(`episode_candidate_document`, over `EpisodeCandidateProjectionV1`). This is a
correctness requirement: the score key hashes the `content_hash` of the composed
search text, so a second implementation would silently score every episode zero.
`the_disk_and_runtime_projections_of_an_episode_agree` and
`both_paths_produce_the_same_index_score_key` pin it.

The projection defaults every field and skips a file it cannot read or parse,
so a record shape that changes costs recall on one episode instead of failing
the whole rebuild.

Episode writes are incremental. `append_native_episode` records a
`MemoryIndexChange::Episodes { agent_id }` in the change journal, which
resolves to an incremental target covering that agent's whole episodic surface
— per-episode goals mean there is no smaller unit worth re-reading, and
reloading it is a directory walk rather than a scope rebuild. A journal write
that fails still marks the scope dirty, so the fallback is the next dirty-scope
rebuild (15–30 minutes by design) rather than an episode missing from the index.

Score invalidation for that target matches on agent plus tier name, skipping
the goal segment, because episodes for one agent span every goal. Matching too
little is the dangerous direction: it would retain a relevance score for
content that just changed.

An agent whose definition is absent from the scope resolves to no target and
takes the full-rebuild path, as the tier arm does for an unknown agent. Episodes
changes must load the scope's definition records for that check, or every
episodes-only snapshot is unresolvable and forces a full rebuild.

Deleting episodes records the same change an append does, so `search_memory`
never returns text from a deleted file.

The audit preview on an episode candidate (`outcome_summary_preview`) mirrors
magician's `truncate_text_for_audit`: whitespace collapses to single spaces so a
multi-line summary stays one readable line, and an elision is marked. It is not
`truncate_projection_text`, which trims without collapsing and is right for its
own single-line inputs.

The index loader applies the runtime's own integrity checks before indexing a
file: the record's `agent_id` must match the directory it sits in, and its
`goal_key` must be non-empty. The directory is not proof of ownership — a
restored backup or hand-copied file can name a different agent — and an index
that accepted what `validate_loaded_native_episode` refuses would be the softer
path to another agent's episodic record.

## End-To-End Architecture

The memory path separates authoritative state, request-time state, and durable
search acceleration so that a slow or damaged derived index cannot change the
answer:

1. **Ingest and canonical commit.** Observation, chat, communications, agent,
   and explicit user-memory flows normalize their results into scoped user,
   agent, or agent-goal tier JSON. Storage writes use the existing atomic-file
   and advisory-lock path. These canonical files are the only memory authority.
2. **Mutation journal.** After the canonical commit succeeds, the writer records
   a generation-tagged, source-addressable entry in `index/changes.json` and
   emits a best-effort in-process notification. Journal persistence is the
   correctness boundary; the notification only reduces refresh latency.
3. **Prompt snapshot refresh.** The notification coalesces writes for 100 ms and
   rebuilds any warm immutable prompt snapshot for that storage root. Every
   request also compares canonical file length and modification stamps, so
   cross-process or out-of-band changes cannot rely on the notification. A
   rebuild reads a consistent before/after source generation and retries if a
   canonical file changes during construction.
4. **Derived index maintenance.** The background maintainer snapshots the same
   durable journal and reloads only affected sources. It updates candidate-level
   `documents.jsonl`, chunk-level LanceDB rows, the embedding cache, temperature
   overlay, and manifest. It acknowledges only the generations it successfully
   applied; a racing newer generation remains pending.
5. **Retrieval.** A prompt request embeds the query and uses LanceDB hybrid
   scores when the manifest/table is compatible. Source-addressable pending
   journal entries do not invalidate the entire last-known-good snapshot: stale
   scores for the affected source are removed, its current canonical rows
   continue through direct lexical ranking, and unrelated LanceDB scores remain
   usable. Structural/full-scope deltas and incompatible or unreadable indexes
   still select direct ranking. Temperature state and eligible hot projections
   are applied after candidate loading, followed by ranking, exact duplicate
   check, semantic-lane selection, and character/entry budget packing.

   Request-path caches (on by default; YAML keys and env kill switches are in
   Embedding Provider):
   - **Hybrid result cache** — revision-bound score map keyed by storage root,
     visibility predicate, embedding contract, index generation, pending-change
     identity, scoring contract, and vector-search ranking epoch. Each request
     captures vector-search settings once so a mid-flight config reload cannot
     disagree with the Lance vector leg. ANN uses a distinct scoring contract
     from flat/shadow. IVF_PQ create and ANN parameter changes bump ranking
     epoch (manifest generation unchanged). ANN fail-closed to exhaustive KNN
     is served but not retained. A hit skips journal I/O, manifest inspect,
     query embedding, and both Lance legs. Failures, timeouts, direct
     fallbacks, stale overlays, and cancelled leaders are never retained. A
     hard-stale inspect drops retained scores; a transient Lance timeout does
     not. Index generation is monotonic by `rebuilt_at`.
   - **Query-vector LRU** — exact contract-plus-query identity; a hit still
     runs Lance. Distinct waiters already queued behind a same-contract waiter
     share one `/api/embed` with the **tightest** deadline; a lone chat miss
     never waits the gather window (`embedding_query_batch_window_ms: 3`,
     `max_items: 8`).
   - **Vector leg** — ANN default: IVF_PQ shortlist of
     `limit * candidate_multiplier`, exact L2 rerank, flat fallback if IVF is
     missing or errors. Missing IVF is not a stale-index reason. IVF/rerank
     leave slack inside the 750ms Lance timeout; a slow ANN query fail-closes
     to flat or empty vector keys and still fuses with FTS.
     `MAGICIAN_VECTOR_SEARCH=flat` restores exhaustive KNN. `ann_shadow` serves
     those flat keys and records recall@k after FTS+flat (observe probe ≤50ms,
     does not delay serve). Cache keys include `candidate_multiplier` and
     `nprobes`. Concurrent identical queries singleflight when the in-memory
     generation token is missing; unpublished loads are not retained.
   - **Table pool** — two independent handles for the concurrent vector and FTS
     legs, atomic to one idle generation or one directory epoch, so RRF cannot
     fuse mixed snapshots. Success returns handles to a generation-keyed idle
     pool; cancel/timeout/error discard them. Rebuilds, incremental writes,
     optimize, quarantine, and hard-stale inspects invalidate the pool,
     including *before* directory replace/recreate/quarantine.
6. **Result and feedback.** The selected bounded prompt block enters the model
   context. Retrieval/selection telemetry and usage signals are persisted by an
   ordered background worker, so they do not extend the response critical path.
   Later utility and outcome signals can change temperature or hot projections;
   they never rewrite the canonical fact merely to accelerate a read.

There are two deliberately different maintenance operations:

- **Reindex/rebuild** reconstructs derived candidate/chunk state from all
  canonical sources. The explicit rebuild endpoint/CLI may atomically replace a
  missing or incompatible LanceDB directory. Runtime reconciliation is
  merge-only and fails closed to direct retrieval when replacement is needed.
- **Optimize/compact** runs LanceDB's explicit optimize action to compact small
  files, prune safe old versions, and incorporate recent row merges into search
  structures. Runtime source deltas do not compact automatically. Operators run
  `memory-index optimize` on a suitable schedule or when status/latency
  indicates it is useful.

Failure behavior is monotonic: canonical commit success makes the new memory
visible immediately; prompt snapshots and indexes can lag or be discarded, but
a pending source delta removes its old derived scores before a result is
returned. Every retained score is keyed by the exact canonical source revision
represented by the manifest. A write arriving after the second journal snapshot
cannot transfer an old score to its new content during the canonical handoff.

## Prompt Read Snapshots

Normal no-cutoff prompt requests use a bounded, process-local immutable snapshot
of canonical candidate documents and exact precomputed lexical features. It
precomputes normalized tokens, lowercase text, packed fuzzy n-grams, exact
deduplication n-grams, character counts, and a symmetric exact-overlap graph
once per canonical source generation instead of once per chat turn.
Query-specific ranking still decides which member of an overlap set wins, and
hot-projected text is compared directly against accepted entries, so the final
exact substring duplicate predicate and output ordering are preserved.

Concurrent user and agent renders also share one immutable effective-temperature
tier map for the same overlay within a one-second turn window. The underlying
score inputs decay on day-scale intervals and are rounded to three decimals; any
persisted overlay update installs a new `Arc` and therefore misses the
disposable map immediately.

The snapshot key includes storage root, agent, scope, goal, environment-memory
policy, and the tier-definition hash. Per-key async build locks prevent a cold
request stampede. Entries are evicted by idle age and oldest use, and total
estimated memory plus entry count are both bounded. Existing `Arc` readers may
briefly retain an old generation while a replacement is installed.

Configure it under `memory.prompt_snapshot` in `magician-config.yaml`:

| Property | Default | Notes |
| --- | --- | --- |
| `enabled` | `true` | Set false to rebuild canonical candidates and features per request. |
| `max_bytes` | `67108864` | Process-local estimated cache bound (64 MiB). |
| `max_entries` | `8` | Maximum distinct scope/agent/goal snapshots. |
| `idle_ttl_secs` | `600` | Unused snapshots are evicted after ten minutes. |
| `refresh_debounce_ms` | `100` | Coalescing window for eager in-process refresh after canonical commits. |

Requests with a recency cutoff bypass this cache because their candidate set is
request-specific. On a cold start or after eviction, the first request builds
one snapshot; later requests validate lightweight source stamps and reuse the
immutable data.

The LanceDB table is chunk-level while `documents.jsonl` remains candidate-level.
Each chunk row carries `chunk_key`, parent `candidate_key`, `chunk_index`,
source char offsets, tier/source metadata, `chunk_text`, `search_text`, and the
embedding vector. Query results are aggregated back to the parent candidate key
before prompt ranking.

## Rebuild And Status APIs

The runtime exposes HTTP endpoints that resolve the active scope from the
workspace-bound Magician bearer.

```text
GET  /api/magician/v2/memory/index/status
POST /api/magician/v2/memory/index/rebuild
```

`status` recomputes current candidate-snapshot hashes and compares them with
the manifest. Missing/corrupt/incompatible indexes are hard-stale and callers
must fall back to direct candidate extraction until `rebuild` succeeds.
`document_count_changed` is soft-stale: canonical tier JSON remains the source
of truth, but the existing LanceDB index can still rank already-indexed
candidates while missing new candidates fall back to direct scoring.
`source_hashes_changed` is hard-stale because old vectors can mis-rank changed
memory content. The manifest hashes stable candidate identity/content fields
that were actually written to `documents.jsonl` and LanceDB, excluding volatile
load-time timestamps. For non-empty manifests, full freshness inspection also
opens the LanceDB candidate table and validates its schema plus required
chunk-key and candidate-key BTree indexes and the `search_text` FTS index type
before trusting the manifest; missing table/index/schema entries and failed
health probes are repairable hard-stale conditions. LanceDB health-check
timeouts are transient unavailability: prompt-time retrieval falls back to
direct ranking and the periodic maintainer skips repair until the table
responds again.

`rebuild` is an explicit operator action. It walks current agent definitions
plus persisted user, agent, and agent-goal memory. When the existing LanceDB
table has the current schema, rebuild uses row-level `merge_insert` on
`chunk_key`: changed rows update when `row_hash` differs, new rows insert, and
target rows missing from the current source snapshot delete. Missing or
incompatible tables fall back to a temporary sibling directory that is promoted
into place atomically. Rebuild also skips row-level merge and fully replaces
LanceDB when the previous manifest is missing, unreadable, has an older
index/schema/backend version, or was built with a different embedding
provider/model/dimension set. Embedding work is progressive either way:
unchanged chunk vectors are loaded from `embedding_cache/`, and only cache
misses call the embedding provider. An explicit rebuild snapshots the durable
source journal before it starts and acknowledges exactly those generations only
after every derived artifact is committed. A canonical write that races the
rebuild carries a newer generation and remains pending. Full status reports
`pending_changes` as hard-stale, so `--skip-if-fresh` cannot strand an
already-reconciled journal entry while request-time hybrid retrieval remains
disabled.

The automatic runtime maintainer never replaces existing derived data. It uses
`reconcile_scope_memory_index`, which permits row-level merge, insert, and
delete against a compatible table. The one safe bootstrap exception is a
missing manifest when the LanceDB directory contains no table: runtime may
create the first table, including stable empty metadata for a canonically empty
scope. A missing manifest beside any unknown/existing table, incompatible
schema/model/dimension, corrupt table, or failed merge remains fail-closed and
requires an explicit rebuild. Runtime never renames or removes `lancedb/`.

Normal canonical writes do not wait for, or trigger, that whole-source walk.
`save_user_knowledge` and `save_native_tier` first commit their JSON source and
then append a source-addressable journal entry. After the dirty debounce, the
maintainer reloads only the changed user-memory set or native tier, replaces its
candidate documents in `documents.jsonl`, and merges/deletes only its LanceDB
rows. A BTree index on `candidate_key` keeps the old-row lookup bounded to the
affected source. The journal is acknowledged only after the derived documents,
LanceDB rows, and manifest are successfully updated. The user-memory set
includes the four legacy files plus every distinct non-legacy `scope: user` tier
declared by the active agent definitions. Shared tier names are represented
once with a deterministic schema owner. Journal read/write/ack operations use a
short cross-process advisory lock (bounded to five seconds); a failed journal
write still emits the legacy dirty notification instead of blocking the
canonical memory save. Pending journal entries are also consumed during the
maintainer's startup pass.

Definition changes, explicit forget/delete operations, unsupported source
shapes, and recovery from a failed source delta trigger a whole-source audit,
but the resulting LanceDB mutation remains row-level. Missing or incompatible
derived state requires the explicit rebuild command. The periodic scan is a
low-frequency audit for manual or out-of-band file edits that cannot create a
journal entry.

The Magician binary also exposes an offline operator path that uses the same
scoped storage layout and rebuild code:

```bash
./magician.bin memory-index status --principal anonymous --workspace default
./magician.bin memory-index rebuild --principal anonymous --workspace default
./magician.bin memory-index rebuild --principal anonymous --workspace default --skip-if-fresh
./magician.bin memory-index rebuild --principal anonymous --workspace default --skip-if-fresh --force
./magician.bin memory-index optimize --principal anonymous --workspace default
```

The Makefile wraps the common cases:

```bash
make memory-index-status
make memory-index-prewarm
make memory-index-rebuild
make memory-index-optimize
```

`memory-index-prewarm` calls `rebuild --skip-if-fresh`, so it remains an explicit
operator action and may replace incompatible derived state. Normal
`make run-supervisor` startup does not invoke it. Set
`MAGICIAN_MEMORY_INDEX_PREWARM=1` only when an operator intentionally wants that
pre-start rebuild. Hard rebuilds are opt-in: `--force`, or
`MEMORY_INDEX_REBUILD_FLAGS=--force` for a one-off `make memory-index-prewarm`.
The Make targets default to `anonymous/default`; override with
`MEMORY_INDEX_PRINCIPAL=<principal>` and `MEMORY_INDEX_WORKSPACE=<workspace>`.

## Embedding Provider

The derived LanceDB table uses local semantic embeddings when Ollama is
available. The default model is:

```text
hf.co/mykor/pplx-embed-v1-4b-GGUF:Q6_K
```

The model contract is configured only under `runtime.ollama` in
`magician-config.yaml`:

| Property | Notes |
| --- | --- |
| `embedding_base_url` | Dedicated embedding-only Ollama endpoint. Default `http://127.0.0.1:11435`. |
| `embedding_keep_alive` | Must be `-1`; the startup helper prewarms and pins the embedding model for the daemon lifetime. |
| `embedding_num_parallel` | Verified physical admission capacity. Currently required to be `1` because the Ollama embedding runner is single-sequence; the client provides foreground priority between requests. |
| `embedding_max_loaded_models` | Must be `1`; the daemon serves only the configured embedding model. |
| `embedding_query_timeout_ms` | End-to-end priority query-embedding budget, including scheduler admission and HTTP. Default `5000`. |
| `embedding_query_batch_window_ms` | Extra gather window only when another same-contract waiter is already queued. Magician default `3`. Capped at `50`. A lone waiter never waits. |
| `embedding_query_batch_max_items` | Distinct queries per physical `/api/embed` call. Magician default `8`. `1` is pass-through. |
| `embedding_query_batch_max_chars` | Character budget for one query embedding HTTP batch. Default `6000`. |
| `embedding_write_timeout_ms` | Per background embedding-batch admission + HTTP budget. Default `180000`. |
| `embedding_model` | Ollama model recorded in the manifest. Changing it makes the index stale. |
| `embedding_dimensions` | Expected vector width; a mismatch fails the embedding call. |
| `embedding_context_tokens` | Ollama `num_ctx` for embedding requests. |
| `embedding_batch_tokens` | Ollama `num_batch` evaluation buffer and physical per-input ceiling. Changing it invalidates vectors built with different fragmentation. |
| `embedding_batch_size` | Initial candidate-text count per `/api/embed` request. A retriable failure halves the working width; four consecutive clean batches double it back, capped at this value. See [Batch width](#batch-width). |

### Batch width

The working width starts at `embedding_batch_size` and moves in both
directions. A retriable Ollama failure halves it; four consecutive clean
batches double it back, never above the configured value, with the success
counter reset at each step so climbing from 1 to 32 takes five sustained runs
rather than one lucky batch. A failure resets the counter, so a flapping
endpoint settles at a workable width instead of oscillating and paying a
failed oversized request every few batches.

Recovery matters for long jobs: a transient timeout early in a full-corpus
rebuild must not pin every remaining batch at a reduced width. The progress
line reports `active_batch_size` next to `configured_batch_size`; a gap means
the width backed off and is climbing back.

One width serves `EmbeddingPriority::{Read, BackgroundRead, Write}`, but it
binds only when a call carries many inputs. A single foreground query embeds
at width 1 whatever this is set to, and the query micro-batch is separately
capped by `embedding_query_batch_max_items`.

Of these, only `embedding_model`, `embedding_dimensions`,
`embedding_context_tokens` and `embedding_batch_tokens` enter the embedding
contract recorded in the manifest (with the input-preprocessing version).
`embedding_base_url` is execution-only: the same model at another endpoint
produces the same vectors, so moving the daemon never marks an index stale (a
unit test pins the endpoint-free contract id). Protected app-memory partitions,
which must not inherit vectors across an endpoint substitution, carry the URL in
`MemoryEmbeddingPhysicalIdentity` instead.

Environment variables are emergency operational overrides; normal deployments
use the config values above:

| Variable | Default | Notes |
| --- | --- | --- |
| `MAGICIAN_MEMORY_EMBEDDING_PROVIDER` | `auto` | `auto` or `ollama` for runtime use. Both require real Ollama embeddings for non-empty rebuilds; if the model is unavailable, rebuild fails and runtime retrieval falls back to direct ranking. `test_hash` is accepted only when the crate is compiled with its explicit test-only feature. |
| `MAGICIAN_MEMORY_OLLAMA_URL` | `runtime.ollama.embedding_base_url` (`http://127.0.0.1:11435`) | Emergency endpoint override, including container-to-host routing. The generic generation URL is only a final compatibility fallback. |
| `MAGICIAN_OLLAMA_EMBEDDING_TIMEOUT_MS`, `MAGICIAN_MEMORY_EMBEDDING_TIMEOUT_MS` | `runtime.ollama.embedding_write_timeout_ms` (`180000`) | Emergency per-write-batch override. Writes do not share the generation daemon or queue. |
| `MAGICIAN_OLLAMA_EMBEDDING_WARMUP_TIMEOUT_MS`, `MAGICIAN_MEMORY_EMBEDDING_WARMUP_TIMEOUT_MS` | `300000` or the rebuild timeout, whichever is larger | Best-effort one-time model warmup request timeout before a non-empty rebuild embedding pass. A warmup failure is logged, then the normal batch loop still runs. |
| `MAGICIAN_MEMORY_QUERY_EMBEDDING_TIMEOUT_MS` | `runtime.ollama.embedding_query_timeout_ms` (`5000`) | Emergency end-to-end priority query-embedding budget override. A timeout immediately selects direct ranking. |
| `MAGICIAN_MEMORY_EMBEDDING_MAX_BATCH_CHARS` | `6000` | Approximate text-volume cap per rebuild embedding request. A single large chunk can exceed this, but multiple chunks are not packed past the cap. |
| `MAGICIAN_MEMORY_EMBEDDING_RETRIES` | `2` | Extra retries for a single-chunk transient timeout, 429, or 5xx after the batch has already shrunk to one item. Query embeddings do not use these retries. |
| `MAGICIAN_MEMORY_EMBEDDING_RETRY_BACKOFF_MS` | `1500` | Linear backoff base for single-chunk retries. |
| `MAGICIAN_MEMORY_EMBEDDING_WARMUP` | `true` | Set to `false` to skip rebuild warmup. Prompt-time query embeddings always skip warmup. |
| `MAGICIAN_MEMORY_LANCEDB_RETRIEVAL_TIMEOUT_MS` | `750` | Per request-time LanceDB FTS/vector budget after the query vector exists. Timeout falls back to direct ranking and opens a short hybrid-retrieval suspension. |
| `MAGICIAN_MEMORY_LANCEDB_HEALTH_CHECK_TIMEOUT_MS` | `5000` | Timeout for prompt/maintenance freshness health checks that open the LanceDB table and list schema/index metadata. Timeout is treated as transient LanceDB unavailability, not corruption. |
| `MAGICIAN_MEMORY_LANCEDB_OPTIMIZE_TIMEOUT_MS` | `60000` | Timeout for the explicit `memory-index optimize` command. Runtime merges do not optimize. |
| `MAGICIAN_MEMORY_INDEX_WRITE_LOCK_TIMEOUT_MS` | `120000` | Timeout for the cross-process advisory lock guarding LanceDB rebuild, quarantine, replacement, merge, and manual optimize writes. |
| `MAGICIAN_MEMORY_HYBRID_RESULT_CACHE` | YAML `runtime.retrieval.result_cache.enabled` (`true`) | Restart-bound. `on`/`enabled`/`1`/`true` enables the score-map cache; `off`/`pass_through`/`0`/`false` is the kill switch even when YAML is true. |
| `MAGICIAN_LANCE_TABLE_POOL` | YAML `runtime.retrieval.lance_table_pool.enabled` (`true`) | Restart-bound. `off`/`pass_through` disables cross-request table reuse. A cold hybrid still opens two tables; a warm hybrid opens none. |
| `MAGICIAN_QUERY_VECTOR_CACHE` | YAML `runtime.retrieval.query_vector_cache.enabled` (`true`) | Restart-bound. Exact-query embedding LRU. `off` is the kill switch. |
| `MAGICIAN_VECTOR_SEARCH` | YAML `runtime.retrieval.vector_search` (`ann`) | Restart-bound. `flat` / `ann_shadow` / `ann`. Invalid values are ignored. Magician default is ANN; `flat` restores exhaustive KNN. |
| `MAGICIAN_EMBEDDING_QUERY_BATCH` | YAML `runtime.ollama.embedding_query_batch_*` (window `3`, max items `8`) | Restart-bound. `off` forces pass-through even if YAML raises the window or item cap. A lone waiter never waits the window. |
| `MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS` | unset | Test-only escape hatch. Set to `1` with `MAGICIAN_MEMORY_EMBEDDING_PROVIDER=test_hash` only in test targets that compile `magician-vector-index` with `test-hash-embeddings`. Runtime builds do not enable that feature. |

When Ollama is used, the manifest records `embedding_provider: "ollama"` plus
the exact `runtime.ollama.embedding_model` and
`runtime.ollama.embedding_dimensions` values. Both are required; there is no
compiled model or dimension fallback. The query path uses the same fields.
Query embedding happens before the LanceDB retrieval deadline; if Ollama is
slow or down, prompt rendering treats the hybrid scorer as unavailable and
falls back to direct ranking without marking the table corrupt. The LanceDB
timeout covers FTS/vector reads only after the query vector exists. Those reads
are lazily constructed and polled in a fresh scheduler task immediately before
LanceDB builds its DataFusion plan. Caller cancellation and retrieval timeout
both abort the isolated search rather than detaching it.

Embedding admission is process-wide and foreground-first. With the verified
single-sequence runner, an already-running provider request is not cancelled,
but analysis and index writes release admission after every small HTTP batch so
a queued retrieval is next. Magician fails configuration closed above capacity
one until launcher/runtime verification can prove real multi-sequence
execution.

Every logical embedding input is constrained by the smaller of
`embedding_context_tokens` and `embedding_batch_tokens` (the runner rejects a
physical prompt larger than `num_batch` even when it fits `num_ctx`). Oversized
UTF-8 text is split into semantic fragments; returned vectors are checked for
count, dimension, and finite values; fragments are length-weighted and
normalized into one logical vector. Requests set `truncate: false`. The
embedding contract and cache namespace include both limits.

Exact prompt-time query embeddings are coalesced across concurrent memory and
procedure retrieval. A successful result is reusable for 25 ms under a
128-entry process-wide bound. Provider failures are removed immediately; every
reuse key includes the complete embedding contract.

Document embeddings are cached under
`memory/index/embedding_cache/ollama-<model>-<dimensions>/`, keyed by the exact
composite `search_text` sent to Ollama. Model or dimension changes use a
different namespace. Production does not write deterministic fake vectors. A
failed embedding call leaves the hybrid index missing or stale, records a
rebuild failure, and keeps direct ranking as the fallback.

## Continuous Maintenance

`MemoryIndexMaintainer` runs inside the Magician service. It accepts best-effort
dirty notifications from scoped memory writes and scoped agent-definition
changes, coalesces repeated writes by `(principal, workspace)`, waits for a
debounce window, and then applies the matching durable source-journal delta for
that scope. Structural journal entries and unknown writes trigger a canonical
source audit followed by merge-only reconciliation, never a full replacement.
Continuous writes cannot reset the timer forever: the first dirty timestamp has
a debounce-derived maximum wait of 15–30 minutes. The normal retry cooldown
still wins. It also periodically scans scoped workspaces, calls
`inspect_scope_memory_index`, and reconciles compatible stale indexes. This
covers new or updated memory tier JSON, deleted/pruned entries, manual file
edits, definition changes that alter tier schemas, and embedding
model/provider changes.

**Runtime placement.** The maintainer is spawned on the dedicated `magician-bg`
tokio runtime (the same runtime as the agent-supervisor sweeps), not the main
request runtime. It is started just after the supervisor loops, so its
dirty-notification sender is registered slightly later in startup; any dirty
mark emitted during that brief window is picked up by the first periodic sweep.

**Request-path search concurrency.** Prompt-time hybrid retrieval
(`score_lancedb_index` / `score_lancedb_hybrid_index`) is gated by a
process-lifetime semaphore that caps concurrent request-path searches. Index
maintenance runs on `magician-bg` and never passes through these scorers. A
search never fails because of the limiter; it only waits for a permit. Lance
offloads CPU/IO onto an internal `lance-cpu` runtime, so no `spawn_blocking`
wrapper is used here. Operators can tune that pool with `LANCE_CPU_THREADS`
and `LANCE_IO_CORE_RESERVATION`.

Configuration:

| Variable | Default | Notes |
| --- | --- | --- |
| `MAGICIAN_MEMORY_INDEX_MAINTAINER` | enabled | Set `0`, `false`, `off`, or `disabled` to turn off the worker. |
| `MAGICIAN_LANCE_SEARCH_CONCURRENCY` | `4` | Restart-bound kill switch. A valid positive integer is the request-path hybrid/FTS lance search permit count and wins over the boot plan. Unset or invalid values use `configure_lance_search_concurrency` (`runtime.scale` `lance_search_cost_units`, default 4). Does not affect index maintenance. |
| `MAGICIAN_MEMORY_INDEX_STARTUP_DELAY_SECS` | `15` | Delay before the first scan after service start. |
| `MAGICIAN_MEMORY_INDEX_INTERVAL_SECS` | `120` | Periodic scan interval. |
| `MAGICIAN_MEMORY_INDEX_DEBOUNCE_SECS` | `15` | Delay after the most recent dirty event before rebuilding that scope. Multiple writes within this window coalesce into one rebuild. |

Empty/uninitialized scopes clear stale cooldown/suspension state only after a
full canonical inspection proves that they contain zero candidates. They then
continue through normal safe bootstrap, which writes a stable zero-document
manifest/documents snapshot and acknowledges the exact pending journal
generation only after every derived artifact commits. Writes that race the
reconciliation retain a newer journal generation. The exact isolated eval
principals `live-eval` and `storage-live-eval` remain owned by their eval
runners and are not adopted by the long-lived production maintainer.

**Derived-index failure cooldown.** A failed scope refresh persists
`memory/index/rebuild-cooldown.json` with its failure count, next eligible
retry time, and a bounded error summary. The schedule is 15 minutes, then 30
minutes, then one hour for subsequent failures. It survives service restarts;
an expiry permits one half-open retry, and a successful refresh removes the
file. While a cooldown is active, LanceDB hybrid retrieval is suspended for
that scope and prompt rendering uses direct ranking immediately. Canonical
memory writes and normal message ingestion continue unaffected. Failures
observed by the startup or periodic audit also enqueue that scope on the dirty
retry loop.

**Ollama/LanceDB stability profile.** Generation remains on the daemon at
`:11434` (the `*local_generation_model` pin). The embedding model is isolated on `:11435`,
pinned with `keep_alive: -1`, and conservatively admitted as one physical
sequence. The supplied live profile keeps per-batch retries and recurring-write
warmup disabled, `MAGICIAN_MEMORY_INDEX_INTERVAL_SECS=21600`, and
`MAGICIAN_MEMORY_INDEX_DEBOUNCE_SECS=1200`. The six-hour periodic pass is only
an audit for manual/out-of-band file edits; normal scoped writes use dirty
notifications and the debounce path. The five-second query budget covers
observed resident-model tail latency while remaining bounded; direct memory
ranking takes over when it expires. Prompt-time Ollama query embedding timeouts
and busy-server responses such as 429/5xx open a short hybrid-retrieval
suspension. 401/404/model-not-found responses are not treated as transient
capacity signals.

Definition source hashes are computed from the stable definition YAML bytes, not
from serialized Rust structs. This avoids false `source_hashes_changed` reports
from unordered map serialization.

Removal is source-driven: delete or prune the canonical memory JSON, then let
the maintainer reconcile. Row-level `merge_insert` deletes target rows missing
from the current source snapshot, including the final row, without replacing
the LanceDB directory. Incompatible tables remain disabled until an explicit
rebuild. Cached embedding files may remain on disk as harmless derived cache
entries.

The maintainer emits `memory_index_reconcile_started`,
`memory_index_reconcile_completed`, and `memory_index_reconcile_failed`
memory-event rows. The payload includes `provider`, `model`, `dims`,
`doc_count`, `duration_ms`, `stale_reason`, dirty reasons when applicable, and
`error` for failures. Failure rows and WARN logs preserve the full error chain.
Repairable LanceDB read errors suspend hybrid retrieval and request merge-only
reconciliation. Runtime never quarantines or renames the index directory;
quarantine/rebuild and optimize remain explicit operator commands protected by
the same in-process mutex and cross-process advisory lock.

Reconciliation does sweep the quarantine pile, under those same locks, before
it reconciles. A quarantine is a forensic copy of a *rebuildable* index, so at
most `MAX_LANCEDB_QUARANTINE_DIRS` (3) `lancedb.corrupt-*` directories are kept,
newest first; everything else is removed best-effort and never fails the
reconcile it rides along with. The bound must be enforced by maintenance, not
only at quarantine time, because the quarantine-time prune stops firing exactly
when the index stops corrupting.

The maintainer's `"memory index maintainer completed"` summary is level-gated.
A steady-state tick that reconciled nothing (`scopes_rebuilt=0
scopes_failed=0`) logs at DEBUG. A tick that reconciled or failed a scope logs
at WARN. LanceDB's internal per-index-part load traces (`type=load_scalar_part
index_type=inverted part_id=N`) are INFO-level library noise; the service pins
`lance` and `lance_index` to WARN in `init_tracing` (overridable via
`--log-level lance=debug,...`).

When `MAGICIAN_MEMORY_EMBEDDING_PROVIDER=auto` cannot reach Ollama during a
non-empty reconciliation, the update fails (`memory_index_reconcile_failed`).
Prompt rendering emits per-request `memory_retrieval_fallback` rows when it
uses direct ranking. Human logs are episode-based: one warning opens the scoped
memory/procedure fallback episode, five-minute summaries report suppressed
counts, and a successful hybrid read emits an explicit recovery message.

LanceDB is a disposable acceleration layer. Repairable request-time read
failures (a corrupt fragment such as `failed to fill whole buffer`, retrieval
timeout) disable hybrid retrieval for that `(principal, workspace)`, mark the
index dirty, and use direct ranking; corruption that prevents opening or merging
requires an explicit operator rebuild. `GET /api/magician/v2/memory/index/status`
is lightweight (manifest + file health, short page-load timeout); exact source
freshness stays in the maintainer and explicit rebuild.

Refreshes project `chunk_key`, `row_hash` and `embedding_input_hash`; exact
matches reuse rows, and the write report records `embedded_rows` and
`reused_rows`.

The LanceDB table carries `chunk_key`, `candidate_key`, `row_hash`, and
`embedding_input_hash`. BTree indexes on `chunk_key` and `candidate_key` back
merge joins and source-delta old-row lookups, and the FTS index on
`search_text` remains the BM25 leg. LanceDB can search recently merged
unindexed rows. Runtime merges deliberately do not call `optimize`. Background
reconciliation and explicit rebuild/optimize actions emit rows for
`memory_index_lancedb_rows_updated`, `memory_index_lancedb_source_delta_updated`,
`memory_index_lancedb_replaced`, and `memory_index_lancedb_optimize_*`.
Replacement and optimize rows can only come from explicit actions.

Long candidates are shaped before embedding. Normal candidates up to roughly
4,000 characters index as one chunk; larger candidates split into overlapping
chunks. `task_progress.notes` entries are first compacted into deterministic
index digests because those often contain raw execution traces whose full text
belongs in task artifacts, not in a single semantic vector.

## Retrieval Contract

Prompt injection keeps direct candidate extraction as the source of truth. When
the manifest is fresh or only soft-stale, it runs the LanceDB FTS/BM25 leg and
the local-embedding vector leg as two CONCURRENT explicit-projection queries
and fuses them with in-house Reciprocal Rank Fusion (same formula, `k`, and
0-based rank indexing as LanceDB's `RRFReranker`, so scores are unchanged),
then uses the fused hybrid score as the primary relevance signal with only
stable tier/confidence tie-breakers before budget packing. The legs select
their scoring pseudo-columns (`_score`, `_distance`) explicitly: LanceDB's
native hybrid path forces ONE shared projection onto both sub-queries, whose
projectable schemas each contain only their own scoring column. When the index
is missing, hard-stale, or unreadable, prompt rendering falls back to direct
lexical ranking. No deterministic embeddings are used in production retrieval:
the path is either real LanceDB hybrid over local Ollama embeddings or the
direct candidate ranker.

Eval rows report the backend actually used. A requested LanceDB hybrid eval
pass that falls back to direct ranking is recorded as `direct_fallback`, not
`lancedb_hybrid`.

Prompt-time hybrid retrieval failures and stale/missing index skips emit
`memory_retrieval_fallback` rows before the renderer switches to direct
ranking. Prompt block rows record the actual `retrieval_backend`, candidate
count, selected count, dropped count, output size, and any fallback reason in
`payload_json`.

Per-candidate prompt retrieval rows also write the ranking `score` used during
prompt packing. This is a relative relevance score, not a calibrated
probability: LanceDB-backed rows primarily reflect the fused hybrid score plus
stable tier/confidence tie-breakers, while direct-fallback rows use tier
priority, stored confidence, exact phrase/token matches, and fuzzy character
n-gram overlap. Rows with `selected = true` are the memories actually injected
into the prompt; rows with `selected = false` were considered and dropped by
budget/count limits. Eval rows provide the stricter correctness view through
`eval_pass`, `matched_count`, and `best_rank`.

User, agent, and agent_goal prompt injection embed the same relevance query.
`memory_prompt_blocks` exposes `score_hybrid_index_for_prompt` (embed + index
lookup + fallback) and `render_memory_tiers_for_prompt_with_scores_result`
(render from a precomputed score map).
`render_memory_tiers_for_prompt_with_index_result` recomposes from the two, so
single-tier callers are byte-identical. Direct-execution injection
(`inject_memory_tiers_for_direct_execution`) scores once and renders all three
tiers from the shared map. The generic procedure index and the derived memory
index both route query embedding through the same coalescing implementation;
each index still runs its own FTS/vector search, authorization filters, score
fusion, and fallback.

Run-start retrieval is seeded from the agent definition's tier list (the same
field the owner execution profile later copies), so the direct-execution seed
and the executor's run-start hydration both retrieve. On success the seed emits
`Run-start memory seed completed` with the tier count and the character length
of each rendered scope. A zero `tier_count` means the agent declares no tiers;
a populated `tier_count` with a zero scope length means that scope genuinely
rendered nothing. The agent-goal scope reports zero for any run without a
`goal_id`. Run-start retrieval sits inside the autonomous-task deadline budget;
`bootstrap_total_ms` warns above one second.

## Temperature And Projection State

Memory temperature is stored as a separate overlay, not by mutating canonical
tier/user memory files. `memory/index/temperature_overlay.json` tracks semantic
lane, temperature tier, prompt usage counters, outcome counters, utility-review
labels, and last-use timestamps by memory candidate key. Overlay schema v6
carries a bounded applied utility-review run ledger: the leased disk queue may
redeliver after a crash, but replay cannot increment overlay counters twice.
Pending review evidence and retry/dead-letter state live separately in
`memory/index/utility_review_queue.json`. Queue transactions capture their
decision timestamp after load-time normalization, so an expired in-flight lease
is eligible for the first post-restart claim without an artificial retry-cycle
delay.

Prompt rendering reads a process-shared immutable parsed overlay and computes
the exact maintenance-equivalent tier projection without mutating or cloning
the full file. After canonical candidates load, prompt-affecting semantic,
confidence, and supersession fields are checked per entry against the canonical
snapshot. A bounded 30-second prompt-tier cache shares that deterministic
projection across successive turns as well as concurrent user/agent renders.
Cache keys use immutable overlay `Arc` identity, so an in-process save or
externally observed file revision installs a new snapshot and invalidates the
projection immediately. A current entry may influence ranking; a missing,
mismatched, or old-schema entry uses the semantic lane's safe default for that
request. The renderer coalesces a background overlay repair per scope and
limits retries to once every 30 seconds. Canonical lifecycle metadata still
filters superseded candidates immediately, independent of the overlay.

The overlay's `load → modify → save` cycle is guarded by a process-global async
lock (`overlay_write_lock`) in every leaf mutator (`sync_*`, `record_*_usage`,
`record_*_utility_review`, `record_*_supersessions`, `maintain_*`). The lock is
taken only at leaf entry points. Lock acquisition emits high-water wait
diagnostics under `magician::metrics::memory_temperature`, tagged by operation
(`sync`, `prompt_usage`, `outcome_usage`, `utility_review`, `supersessions`,
`maintenance`). `render_memory_section` enforces each lane's `max_entries` at
render time in addition to `max_chars`. The memory utility reviewer `warn!`s
instead of silently no-opping when `operation_llm_router` is absent.

Retrieval/selection/injection usage is serialized by one background persistence
worker after prompt selection. Candidate packing retains the same score,
first-survivor duplicate semantics, and per-lane budgets; duplicate candidates
are prefiltered through an exact substring n-gram index. Direct-fallback fuzzy
character features are losslessly packed into sorted integer vectors and
intersected linearly.

Lane budgets are configured in the active `magician-config.yaml` under:

```yaml
memory:
  prompt_lane_budgets:
    user:
      user_preference: { max_entries: 4, max_chars: 1600 }
    agent:
      procedure: { max_entries: 2, max_chars: 880 }
    agent_goal:
      project_context: { max_entries: 4, max_chars: 2520 }
```

Missing lanes keep the built-in scope defaults. Setting both caps to `0`
disables a lane for that scope. Config reload applies this policy to subsequent
chat, direct-execution, and eval memory renders.

`memory/index/hot_projections.json` stores compact prompt-ready projections
created by the memory utility reviewer for useful/load-bearing memory. The
renderer keeps the source candidate key as the feedback identity but injects the
projection text when the projection is active and its source text hash still
matches canonical memory. Durable maintenance deactivates records for source
hash mismatch, stale verification, policy-version drift, or old never-injected
projections and records the deactivation reason. Projection records carry
deduped source refs, source tier/item metadata, policy version, last
regeneration time, and regeneration count. T0 projection writes are reserved for
stable user-preference, procedure, project-context, and entity lanes; episode
projections compact into T1, and raw source evidence hydrates from canonical
memory instead of being projected.

During consolidation, episode-source prompts include an LLM memory-quality
classification when the operation router is available
(`memory_episode_quality_classification`, default chunked local Ollama
memory-quality profile). Unavailable/invalid JSON falls back to the
deterministic signal. Reviews are cached by a content hash of the exact
episode-source batch (in process and under the scoped index). Successful
classifications survive a restart; failures leave a capped exponential retry
guard. Calls are singleflight per memory scope; batch transforms stay durably
pending when foreground, full-lane, or same-provider dispatch pressure is
visible.

Structured owner-memory writes enter the shared
[memory lifecycle](memory-lifecycle.md). Explicit saves, learned promotions and remembered clarifications preserve
revisions; the `memory_lifecycle_review` operation decides relationships across
keys and owner tiers before guarded apply. Pending/retired revisions are excluded
from current recall, ambiguous claims use durable owner clarification, and
observations cannot silently retire a stated goal. Attention source adapters obey
the same current-memory boundary.

Agent/goal operational-tier similarity-based merges retain their existing LLM operations
(`memory_conflict_review`, or `memory_conflict_review_high_risk` for
legacy global targets) in target-level batches before replacing an existing item.
Normal review routes mini-first with a full-model retry; high-risk user/global
targets stay on the stronger profile. Pairs enter review when they exceed the
text-similarity threshold or share the same durable memory key. Planning
happens before tier/user file locks; the locked write path only applies an
already-computed plan. A reviewed `replace_existing` keeps the old item as
`memory_lifecycle: superseded` evidence, records successor/reason/timestamp,
and appends the incoming item. Missing or failed reviews keep both items;
`UpsertBySimilarity` cannot destructively replace memory without an explicit
reviewed decision. Conflict reviews emit `memory_conflict_review` rows.

LLM consolidation output is normalized before apply: an active insight's
explicit “supersedes/replaces previous value” suffix is removed
deterministically. Conflict, evidence, history, source, `before`, and explicit
supersession audit fields are exempt. Collection-root normalization also covers
older durable records: named wrappers such as `distilled_insights` reduce to
the declared collection, and legacy `{key, value}` object maps (`org_state`,
`product_state`, `gtm_state`) become stable sorted entries. Ambiguous objects
fail closed. Older rich-schema items whose current contract is `{key, value}`
use `name`/`title`/`surface`/`summary` as the key and flatten the old payload
into the value (newest duplicate wins). That migration applies only to existing
durable state under the tier lock; fresh LLM output stays strictly validated
and still uses the one-attempt schema-repair path.

A bounded same-durable-key contradiction sweep for operational tiers reuses that reviewer contract,
marks one side superseded only on explicit replacement, and stamps reviewed
keep-both pairs so they are not re-reviewed. The overlay treats superseded
entries as audit-only: T3 with score `0.0`. Prompt rendering and
`search_memory` skip them; `/memory` still exposes superseded counts, recent
rows, and bounded supersession chains. Operators can trigger the same path
through `POST /api/magician/v2/memory/temperature/maintain`; its user-memory sweep
invokes shared lifecycle reconciliation. If the reviewed
snapshot no longer matches the locked write snapshot, or the reviewer is
unavailable, the merge keeps both items.

Operational verification is split into deterministic slices in
`docs/runbooks/2026-06-14-fractal-memory-temperature-tiers.md`: semantic lanes,
prompt packing, temperature maintenance, utility review, hot projections,
contradictions, API/UI wiring, and optional live API smoke.

### Overlay Retention And Eviction

The retention anchor is `first_seen_at`. `apply_memory_temperature_overlay_sync`
stamps it the first time it sees a live candidate without one, and never moves
it. A `None` anchor is treated as older than any TTL: genuine orphans (no live
candidate) stay immediately collectable. Key migration preserves counters but
does not stamp an anchor.

A Neutral outcome is evidence. `apply_memory_temperature_outcome_usage` stamps
`last_used_at` for every signal; `has_any_temperature_signal` counts that
timestamp, so an entry whose whole history is Neutral is not treated as
never-touched.

Hot projections migrate with the overlay on every path via
`migrate_memory_hot_projection_keys_for_scope` (API handler and index rebuild).
They are keyed by candidate identity; migrating the overlay alone would strand
them.

`load_memory_temperature_overlay_snapshot` and
`load_memory_hot_projection_index_snapshot` cache only when the file stamp is
unchanged across the read.

Lane partitions are borrowed, not rebuilt. `memory_temperature_scope_partition`
returns `Cow` and slices a contiguous candidate-key prefix.

### Supersession Key Namespace

`MemoryTemperatureEntry.superseded_by` must hold a **temperature candidate
key**, because `memory_supersession_chains` resolves it with `entries.get(key)`
against the overlay.

The explicit `apply_memory_temperature_supersessions` API stores a candidate
key. The consolidator's `mark_memory_item_superseded_with_reason`
also stamps `superseded_by_item_key` holding `item_memory_key(incoming_item)` —
the same function the candidate loader names items with.
`superseded_by_for_candidate` prefers it and composes the successor's full key
with `memory_temperature_candidate_key_from_parts` against the *superseded*
candidate's own scope/agent/goal/tier (a replacement is always a sibling).

Resolution order, most specific first:

1. `superseded_by_item_key` — composed here.
2. `superseded_by_candidate_key` — an already-whole candidate key.
3. `superseded_by` / `superseded_by_key` — legacy.

`item_memory_key` returns `None` for an item with no `key`/`name`/`id`-style
field. Nothing is written in that case; the reader falls back to the legacy
value.

**Known bound.** An item's key changes the moment it is marked superseded, so
the superseded row and a replacement reusing the same `key` do not collide. A
chain therefore resolves its first hop; if that successor is itself later
superseded, the earlier pointer no longer names it. Three-deep chains show one
resolved hop and then `successor_missing_from_overlay`.

## Code Knowledge

Semantic lane `SemanticMemoryType::CodeKnowledge` maps tier names
`code_knowledge` / `codebase` / `source_code` / `architectural` *before* the
user-preference `"knowledge"` keyword (`infer_semantic_memory_type` in
`magician-vector-index`). Lane policy is procedure/project-like: small T0,
useful facts heat with evidence; default T1, score 0.62, stale after 60 days;
eligible for hot projections including T0; prompt-order after Project Context.
`MemoryPromptLaneBudgets::for_scope` gives the lane an AgentGoal share and a
smaller Agent share; config-key `code_knowledge` maps it for
`magician-config.yaml` overrides.

It is **not** a default personal-agent tier. Coding workers (`kind: worker`)
skip personal-agent defaults and declare their own `codebase_knowledge` or
`architectural_knowledge` collections (`{key, value, last_seen}` plus
`project_id`). The five coding agents (senior-software-developer,
principal-software-engineer, frontend-engineer, junior-frontend-engineer,
junior-software-engineer) distill via `consolidate_to_codebase_knowledge` or
`consolidate_to_architectural_knowledge` using prompts
`code_distill_system_v1.0.0` and `code_distill_user_v1.0.0`
(`data/magician_v2/prompts/`). Coordinators, architect, and non-coders have no
`run_coding_task` and are out of scope.

**Write path.** Distill output is a top-level `[{key, value}]` array accepted by
`value_for_tier_root_merge`; `upsert_by_name` dedups by `key` via
`durable_memory_key`. An optional `EpisodeProjectResolver` (VibeDev: parse the
stable `VibeDev project: <uuid>` line from the coding task description) stamps
`project_id` onto every fact before `apply_target` when every episode in the
group resolves to the same project. `execute_rule` partitions multi-project
code-knowledge batches per VibeDev project
(`partition_code_batch_by_project`) and runs distill+stamp+apply once per
group. No-op (single call) for non-code rules, no resolver, single-project
batches, or non-`Episodes` sources. Mixed unresolved batches stay global.
`execute_rule_single` redacts unambiguous secret token shapes
(`redact_obvious_secrets`: `sk-`/`rk-`/`pk-` provider keys, `AKIA`/`ASIA`,
`gh*_`, `xox*-`, JWTs, `Bearer <token>`, URL `user:pass@`) from code-knowledge
facts only.

**Read path.** Citizen tool `magician_code_knowledge { query, k? }` →
`POST /vibedev/citizen/code_knowledge` (`citizen_code_knowledge_handler` in
`magician-api/src/vibedev_api.rs`). Bearer = run scope → `CitizenGrant`. The
handler prefers `grant.agent_id` (the executing engineer, set at mint in
`run_coding_task`) and falls back to `grant.root_task_id` → `get_task` →
`manifest.agent_id` only for legacy grants. Resolving via the cockpit root task
alone is wrong: that task is owned by the `engineering-manager` coordinator
and has no CodeKnowledge tier. Retrieval reuses
`rank_memory_candidates_hybrid` (`tier_filter = None`) then post-filters to the
CodeKnowledge lane on `candidate.semantic_memory_type`, drops superseded
overlay entries, records retrieval usage, and returns top-`k` (default 6):
`{ok, facts:[{key, text, tier, score, source, temperature_tier}], backend}`. A
fact is kept when `metadata_json["project_id"]` is absent/empty (global) or
equals the run's resolved UUID. Extension:
`assets/pi-extensions/magician-citizen.ts` (`include_str!`; magician must be
rebuilt to ship). One read targets the run's single agent.

Fresh facts follow `save_native_tier` → durable source journal →
`MemoryIndexMaintainer` source delta. The supplied live debounce is 20 minutes
(`MAGICIAN_MEMORY_INDEX_DEBOUNCE_SECS=1200`).

## Internal Data Observability

The internal diagnostics tool exposes the memory lakehouse directly:

- `memory_observability_summary {hours: 24}` returns recent event/status counts,
  backend counts, selected-vs-dropped relevance score aggregates, low-score
  selected memories, and fallback/failure rows.
- `memory_index_snapshot {}` returns the current manifest, document sidecar
  line count, LanceDB file count, provider/model/dimensions, and stale/failure
  diagnostics.
- `query_memory_events` accepts bounded `SELECT`/`WITH` SQL over the
  `memory_events` view backed by
  `analytics/memory_events/dt=*/*.parquet`.

Useful starting queries:

```sql
SELECT event_kind, status, retrieval_backend, COUNT(*) AS rows
FROM memory_events
GROUP BY event_kind, status, retrieval_backend
ORDER BY rows DESC;
```

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

Future retrieval backends should follow this order:

1. Check the manifest.
2. If fresh, read derived documents or backend-specific indexes.
3. If stale or missing, fall back to direct candidate extraction.
4. Rebuild the index asynchronously or through the operator endpoint.

The manifest backend is `lancedb-hybrid-v2`; schema version 9 uses chunk-level
LanceDB rows with collision-safe candidate keys, row-level merge metadata, a
`candidate_key` BTree index for source-addressable delta updates, vectors bound
to the embedding/input contract, and revision-bearing candidate source hashes
for race-safe score handoff. `documents.jsonl` remains as an auditable
`jsonl-direct` sidecar and rebuild source for future retrieval backends.
Existing schema 5, 6, 7, or 8 indexes are rebuilt once before they can accept
complete source deltas.
