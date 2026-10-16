# Magician Vector Index Changelog

All notable changes to `magician-vector-index` are documented here.

## [Unreleased]

### 2026-09-13 — 0.1.14 — Lifecycle-aware memory candidates

- Recall excludes pending, superseded, retracted and expired memory revisions; stable record IDs keep revision identity across tiers.

### Fixed (0.1.13)

- The embedding contract no longer includes the endpoint URL, so a briefly released version's spurious "rebuild required" on every personal memory index is gone; no rebuild is needed.

### Added (0.1.12)

- Embeddings route through the configured magicllm router when installed, with identical behaviour on the fallback path; health probes honour the caller's timeout.
- Optional ANN vector search (IVF_PQ shortlist with exact rerank), default `ann` in Magician config, plus an `ann_shadow` mode that measures recall; `MAGICIAN_VECTOR_SEARCH` overrides at restart.
- Configurable Lance search concurrency; `MAGICIAN_LANCE_SEARCH_CONCURRENCY` overrides at restart.

### Changed

- Hybrid scoring always deduplicates concurrent loads.
- Batched query embeddings honour the tightest caller deadline and skip cancelled callers.
- A slow ANN query returns no vector hits (full-text still answers) instead of blowing the search timeout; shadow observation cannot delay results.
- Magician config enables the hybrid result cache, query-vector cache and query-embed batching; each has an `=off` environment kill switch.
- The query-embed batching window no longer restarts per waiter.
- Utility-review runs are idempotent across crashes.
- File-lock timeouts are retried rather than treated as corrupt data.

## [0.1.11] - 2026-07-29

### Changed

- Upgraded the Ollama HTTP client with rustls and explicit macOS system-proxy discovery.

---

Older entries: `docs/archive/changelogs/magician-vector-index.md`
