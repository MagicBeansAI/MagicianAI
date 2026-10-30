# Magician Vector Index Changelog

All notable changes to `magician-vector-index` are documented here.

## [Unreleased]

- 0.1.18: Write memory temperature counters behind and reuse unchanged source documents on snapshot refresh.

- Memory temperature usage counters (prompt, retrieval and outcome bookkeeping) are write-behind: applied to the in-process snapshot at once and persisted by one overlay write per `MEMORY_TEMPERATURE_COUNTER_FLUSH_DELAY` (2 s, the crash-loss window for those counters). Durable writes carry pending counters exactly once; `flush_pending_memory_temperature_writes()` persists them at shutdown; `MemoryStorage::shared_handle` opts a storage in.
- Memory temperature overlay mutations clone, mutate and serialize on the blocking pool under the existing write lock, skip the save when nothing changed, and serialize without an intermediate `serde_json::Value`.
- Expose per-episode and per-tier candidate loading so snapshot refreshes can reuse unchanged source documents.
- `examples/lance_hybrid_search_bench` times the prompt-path hybrid search against a memory index copy, before and after compaction; hybrid scoring logs its journal, inspect, search and overlay phases.

### 2026-10-24 — 0.1.17

- Near-duplicate memory folding and passage selection index claims once per render instead of comparing every pair, with the same results. The Lance search timeout is configurable (default 3 s, was 750 ms).
- An empty memory text no longer fails an Ollama embedding batch and switches off hybrid retrieval.

### 2026-10-20 — 0.1.16

- `text_bounds::repair_dangling_cut` repairs a legacy mid-word cut (a word glued to `...`, or a short stub after the last sentence) at read time, ending where `bound_text` would.

### 2026-10-20 — 0.1.15 — Readable, bounded recall

- Episode recall is assembled from verdict, task intent and the run's user-facing output behind a user-facing-prose gate (no payloads, fences, runtime markers, brief replays or bare identifiers), at record and read time; tier items render as `label (as of date): value`.
- `text_bounds`: the one boundary-aware way recall text is shortened (items, word-aligned passages, section caps); `memory_similarity::near_duplicate_claims` for near-duplicate folding.

### 2026-09-13 — 0.1.14 — Lifecycle-aware memory candidates

- Recall excludes pending, superseded, retracted and expired memory revisions; stable record IDs keep revision identity across tiers.

### Fixed (0.1.13)

- The embedding contract no longer includes the endpoint URL, so a briefly released version's spurious "rebuild required" on every personal memory index is gone; no rebuild is needed.

---

Older entries: `docs/archive/changelogs/magician-vector-index.md`
