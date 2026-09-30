# Magician Vector Index Changelog

All notable changes to `magician-vector-index` are documented here.

## [Unreleased]

### 2026-09-13 — 0.1.14 — Lifecycle-aware memory candidates

- Current recall excludes pending, superseded, retracted and expired memory revisions. Stable record IDs preserve revision identity and cross-tier successor lookup without conflating retention temperature with current truth.
- See the [memory lifecycle contract](../docs/components/magician/memory-lifecycle.md#clarification-and-recall).

### Fixed (0.1.13)

- The memory-index embedding contract no longer includes the embedding
  endpoint URL. The same model at another endpoint produces the same vectors,
  and the short-lived v2 contract that folded the URL in (2026-08-24) marked
  every personal memory index stale with an explicit rebuild demanded for a
  change that could not alter one vector. The endpoint-free contract
  reproduces the stored ids exactly, so no rebuild is needed; a unit test pins
  it. Protected app-memory partitions keep the URL in
  `MemoryEmbeddingPhysicalIdentity`.

### Added (0.1.12)

- Routed embedding seam (`embedding_router`): `install_embedding_router` /
  `uninstall_embedding_router` publish the magicllm `ConfiguredRouter` that
  resolves embedding provider identity. `post_ollama_embed` routes through
  the profile bound to `embed_documents`/`embed_query` when installed and
  otherwise constructs an ad-hoc magicllm `OllamaProvider` — the wire body,
  millisecond deadlines, and retry classification (429/5xx retriable, other
  4xx terminal) are identical on both paths, and no `/api/embed` literal
  remains in this crate. The routed health probe honors the caller's
  `health_timeout` (10s default), not the provider's 180s default.

- Optional hybrid vector-leg ANN: Magician YAML default is `ann` (IVF_PQ
  shortlist + exact L2 rerank, flat fallback). Crate `Default` stays `flat`
  so uninstalled unit tests keep `bypass_vector_index`. `ann_shadow` serves
  flat after FTS+flat and records recall@k on remaining budget.
  `MAGICIAN_VECTOR_SEARCH` is the restart-bound override. IVF_PQ is created
  only in `ann_shadow`/`ann` when row count ≥ `ann.min_rows` (default 256).
  Missing IVF is not a stale-index reason. Hybrid cache keys include ranking
  epoch, candidate_multiplier, and nprobes. IVF create drops retained scores
  for that Lance directory.
- `configure_lance_search_concurrency` records the boot-plan
  `lance_search_cost_units` before the first request-path search acquire.
  `MAGICIAN_LANCE_SEARCH_CONCURRENCY` remains the restart-bound kill switch: a
  valid positive integer wins over the configured atomic (default 4).

### Changed

- Hybrid scoring always singleflights, including when the in-memory
  generation token is missing (boot / IVF create). Those loads are not
  retained until inspect republishes a generation.

- Query-embed micro-batch physical calls use the **tightest** waiter
  deadline so a background retrieval cannot keep Ollama occupied after a
  chat miss has given up.
- ANN IVF/rerank leave slack inside the 750ms Lance timeout. A slow IVF
  query fail-closes to **empty vector keys** (FTS still fuses) instead of
  starting unbounded flat KNN that would blow the outer timeout and discard
  FTS. `ann_shadow` observe is spawned on the Lance runtime and capped at
  50ms extra so it cannot delay served hybrid. Query-embed dispatch skips
  cancelled waiters before the physical call. Tiny IVF_PQ tests use 16 rows
  and `num_bits=4` / `num_sub_vectors=2`.

- Magician YAML now enables hybrid result cache, query-vector LRU, and
  query-embed micro-batch (window 3 ms, max items 8). Crate `Default` for
  `HybridResultCacheSettings` and `QueryVectorCacheSettings` stays
  `enabled: false`, and `QueryEmbedBatchSettings` stays pass-through, so
  unit tests that never install Magician config still skip the accelerators.
  Kill switches remain `MAGICIAN_MEMORY_HYBRID_RESULT_CACHE=off`,
  `MAGICIAN_QUERY_VECTOR_CACHE=off`, and `MAGICIAN_EMBEDDING_QUERY_BATCH=off`.
- Query-embed gather window is one-shot per batch: two or more distinct
  same-contract waiters flush at `now + window_ms` instead of restarting the
  window until the query deadline.
- Temperature-overlay schema v5 records a bounded applied utility-review run
  ledger, making leased queue redelivery idempotent across process crashes.
- Memory storage now preserves file-lock timeout as a typed error so durable
  maintenance can retry contention without misclassifying it as poison data.

## [0.1.11] - 2026-07-29

### Changed

- Upgraded the Ollama HTTP client to `reqwest` 0.12.28 with rustls and explicit
  macOS system-proxy discovery, aligning it with the workspace client stack and
  removing the incompatible proxy initialization path.

---

Older entries: `docs/archive/changelogs/magician-vector-index.md`
