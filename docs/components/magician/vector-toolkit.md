# Vector Toolkit

Owner: magician runtime.

## What it is

A generic semantic-vector capability exposed to agents as the `vector` tool. Three sub-actions backed by lancedb + Ollama embeddings:

| Action | Purpose | Persistence |
|---|---|---|
| `vector.index` | Embed + persist items to a named namespace (lancedb table). Upsert-by-id. | Yes — `<scope>/vector_tables/<namespace>/` |
| `vector.search` | Query a persisted namespace by hybrid FTS + vector / FTS-only / vector-only. | Reads persisted state. |
| `vector.rank` | Reorder / cluster / dedupe a flat list of items in-memory. No persistence. | None — stateless. |

Built on the same lancedb infrastructure that powers the agent memory index (`magician-vector-index` crate). 100% in-process Rust — no subprocess.

Exact prompt-time query embeddings are coalesced across concurrent retrieval
branches, keyed by endpoint, model, dimensions, context, batch-token contract and
exact query. Successful vectors stay reusable for a 25 ms sibling window;
failures are evicted immediately. Document/index embeddings instead use
read-first admission and yieldable write batches.

## Why a generic toolkit (not a per-domain provider)

Embedding, hybrid retrieval, semantic clustering and dedup recur across research,
RAG, dedup and topic-detection workflows (catch-up rerank, clustering error
reports, feedback themes). One three-action capability that agents compose beats
one-off providers like `catchup_rerank` or `log_cluster`.

## Architecture

```
┌────────────────────────┐    ┌─────────────────────────────────┐
│  Agent's outer LLM     │    │  magician runtime                │
│  vector(action=..., …) │◄──►│  CapabilityRegistry → Vector     │
└────────────────────────┘    │      ↓                           │
                              │  in-process: magician-vector-    │
                              │  index::VectorTable              │
                              │      ↓                           │
                              │  lancedb (per-namespace dir)     │
                              │      +                           │
                              │  OllamaEmbedder (HTTP /api/embed)│
                              └─────────────────────────────────┘
                                            ↓
                                  ┌─────────────────┐
                                  │ embedding-only │
                                  │ Ollama :11435  │
                                  │ (pinned)       │
                                  └─────────────────┘
```

### Implementation type

`implementation: { type: compiled, provider_name: "vector" }` — same path as `shell`, `http`, `files`, `task_state`. Single-call utility; no LLM round-trip for tool selection. See `magician/src/magician_v2/execution/compiled_providers.rs::VectorCapabilityProvider`.

### Storage layout

```
<scope>/vector_tables/
  <namespace>/            ← lancedb table dir; one per namespace
    items/                ← lancedb internals (Arrow batches, FTS index)
    _versions/
    ...
```

Per-table schema: `(id: utf8, text: utf8, metadata: utf8 [JSON-stringified], embedding: fixed_size_list<f32, dims>)` + FTS index on `text`.

`namespace` is agent-chosen. Use `task_<task_id>` for ephemeral per-research pools; reuse stable names for long-lived caches.

**Security — namespace allowlist:** the namespace is a path segment, so
`is_safe_namespace` (checked at `vector.index` / `vector.search` dispatch)
rejects empty, leading-dot, `..`-containing names and any character outside
`[A-Za-z0-9_.:-]`.

**Index semantics — merge-only at runtime:** the first `vector.index` creates the
table and FTS index; later calls `merge_insert` by `id` (update matches, insert
new, leave others). Runtime indexing never deletes, replaces or optimizes the
namespace. Duplicate IDs within one request are rejected. `VectorTable` also
exposes non-creating existence checks and exact-ID deletion for owned
derived-index maintainers (e.g. procedure indexing); these are not agent-facing.

**Threshold validation:** `vector.rank` requires `threshold ∈ [0.0, 1.0]`
(cosine on L2-normalized vectors); out-of-range is an error rather than a silent
"no clusters"/"one cluster". Default 0.85 for `clusters`, 0.90 for `deduped`.

## Ollama lifecycle

The vector toolkit hard-depends on a running Ollama daemon with the configured embedding model pulled. Magician owns the lifecycle:

### On boot (`magician/src/magician_v2/runtime/ollama_lifecycle.rs::start`)

1. Resolve the dedicated embedding endpoint from
   `runtime.ollama.embedding_base_url` (default `127.0.0.1:11435`).
2. Locate `ollama` binary on PATH (or `/opt/homebrew/bin/ollama`, `/usr/local/bin/ollama`, `/usr/bin/ollama`).
3. Probe configured base URL (`GET /api/tags`).
4. If reachable → use existing daemon (do NOT spawn, do NOT stop on shutdown).
5. If a local endpoint is not reachable → spawn `ollama serve` with the
   configured admission capacity (one verified physical sequence) and one
   loaded-model slot; remote endpoints are never spawned locally.
6. Check that the configured embedding model is installed, then prewarm it with
   `keep_alive: -1`; if missing or residency fails, keep the
   vector capability unavailable and report the configuration error. Runtime
   never pulls or substitutes a model; `make setup-ollama` is the explicit
   installation path.

The canonical `make run-supervisor` path runs the shell lifecycle first. That
layer records its PID and launch signature, restarts mismatched owned daemons,
and can replace a conflicting local daemon before the in-process lifecycle
performs this health/availability sequence. Direct `magician.bin` launches keep
the conservative in-process behavior above.

### While running

A background task polls `/api/tags` every 30s. Recovery prewarms the model before
marking the vector capability available. Retrieval embeddings use the foreground
lane. Optional/background and durable-write requests are deliberately small and
release admission after every provider call; a queued foreground read is then
admitted before more background work. Magician requires the verified
capacity to be one; configuration fails closed rather than assuming Ollama can
run parallel embedding sequences. If availability is false, the `vector` tool
is hidden from new agent catalogs.

### On shutdown (`stop`)

- If magician spawned the daemon → SIGTERM, wait up to 5s, SIGKILL.
- If pre-existing → leave alone.
- The shell stop path (`make stop-supervisor` / `make stop-all`) also runs
  `scripts/stop-ollama.sh`, which asks Ollama to unload currently resident
  models before process teardown. This releases local memory even when the
  daemon itself is user-managed and left running.

### Operator knobs

| Env var | Default | Effect |
|---|---|---|
| `MAGICIAN_OLLAMA_AUTOSTART` | `true` | Set `false` to skip the boot spawn step. Health gating still applies. |
| `MAGICIAN_OLLAMA_UNLOAD_ON_STOP` | `true` | Set `false` to skip the stop-time model unload pass. |
| `MAGICIAN_MEMORY_OLLAMA_URL` | `runtime.ollama.embedding_base_url` (`http://127.0.0.1:11435`) | Emergency dedicated-endpoint override. `MAGICIAN_OLLAMA_BASE_URL` is only a final compatibility fallback. |
| `MAGICIAN_OLLAMA_EMBEDDING_TIMEOUT_MS` | `runtime.ollama.embedding_write_timeout_ms` (`180000`) | Emergency write-batch admission + HTTP timeout override. |
| `MAGICIAN_MEMORY_QUERY_EMBEDDING_TIMEOUT_MS` | `runtime.ollama.embedding_query_timeout_ms` (`5000`) | Emergency end-to-end priority query-embedding timeout override. |
| `MAGICIAN_OLLAMA_HEALTH_TIMEOUT_MS` | `10000` | Per-call timeout for the health probe + model-tag check. |
| `MAGICIAN_OLLAMA_KV_CACHE_TYPE` | `runtime.ollama.kv_cache_type` (`q8_0`) | Temporary daemon KV-cache quantization override. |
| `MAGICIAN_OLLAMA_FLASH_ATTENTION` | `runtime.ollama.flash_attention` (`true`) | Temporary daemon flash-attention override. |
| `MAGICIAN_OLLAMA_PREWARM` | `runtime.ollama.prewarm` (`true`) | Generation-daemon prewarm only; embedding prewarm is mandatory. |
| `MAGICIAN_OLLAMA_REPLACE_EXISTING_LOCAL_DAEMON` | `runtime.ollama.replace_existing_local_daemon` (`true`) | Set `false` to reuse an unowned local daemon without launch-setting verification. Remote endpoints are unaffected. |

Generation tags and contexts remain on the unchanged `:11434` daemon. The shell
lifecycle manages a second PID/signature/log for the embedding-only `:11435`
daemon. Missing models, failed prewarm, or failed residency verification make
startup fail rather than selecting a fallback or reporting degraded success.

The embedding contract is config-only: `runtime.ollama.embedding_model`,
`embedding_dimensions`, `embedding_context_tokens`, `embedding_batch_tokens`,
`embedding_batch_size`, `embedding_num_parallel`, and
`embedding_max_loaded_models`, so the launcher and in-process admission cannot
disagree about physical capacity. Endpoint, residency, timeouts, and
low-level cache/flash tuning remain config-first with the documented emergency
env overrides.

`embedding_context_tokens` is the model-context boundary and
`embedding_batch_tokens` is both the runner evaluation batch and, for Ollama's
llama.cpp embedding path, the maximum physical prompt it accepts. Text that
cannot fit the smaller ceiling is split at UTF-8-safe semantic boundaries,
embedded as bounded physical fragments, and pooled back into one
length-weighted normalized logical vector. Both ceilings are part of the
persisted embedding contract because changing fragmentation can change the
pooled vector. Every request disables Ollama's silent truncation, and bounded
HTTP error details retain provider causes such as physical-batch overflow.
Response count, dimensions, and finite values are validated before a vector is
returned. This applies to vector tools, memory documents, and oversized
retrieval queries; callers still receive exactly one vector per logical input.

Request-path query embeddings may reuse an exact contract-plus-query vector
from a process-wide LRU (`runtime.retrieval.query_vector_cache`, Magician
default on) and may share one physical `/api/embed` call across distinct
same-contract queries that are already queued
(`runtime.ollama.embedding_query_batch_*`, Magician default window 3 ms and
max items 8). Empty-queue dispatch is immediate so a single chat miss does
not wait. When two or more distinct same-contract queries are already
queued, the leader waits at most that window once, then flushes. `MAGICIAN_QUERY_VECTOR_CACHE=off` and
`MAGICIAN_EMBEDDING_QUERY_BATCH=off` are the kill switches. The crate
`Default` for these settings stays off / pass-through until Magician
installs YAML.

Vector-toolkit `SearchMode::Vector` stays exhaustive nearest-neighbor on the
tool table. Optional IVF_PQ / ANN shadow is a memory-hybrid concern
(`runtime.retrieval.vector_search`, default `flat`) and does not change this
agent-facing toolkit.

Operational URL and timeout knobs retain their `MAGICIAN_MEMORY_*` compatibility
aliases. The HTTP client itself is reused across embed / health / tag calls via
a single `reqwest::Client` per `OllamaEmbedder`; embedding budgets cover scheduler
admission plus the remaining HTTP request time, while health probes retain their
independent timeout.

## Agent grants

Granted to **internal-system-analyst** (cluster errors, similar incidents, dedupe
stack traces) and **simple-data-analyst** (cluster feedback, semantic filter on
free text). The web-researcher uses deterministic `catchup_merge` ordering with no
Ollama rerank, so catch-up answers do not depend on local embeddings.
Software-engineering agents are not granted it: general embeddings underperform
on code search.

## Usage examples

### Rerank a flat list against a query

```yaml
vector:
  action: rank
  output: ranked
  query: "best AI video generation tools in 2026"
  limit: 10
  items:
    - {id: "u1", text: "OpenAI Sora 2 launches with public beta", metadata: {url: "..."}}
    - {id: "u2", text: "Runway Gen-4 adds longer clip lengths", metadata: {url: "..."}}
    - {id: "u3", text: "Apple iPhone Air gets thinner camera bump", metadata: {url: "..."}}
```

`u1`, `u2` rank ahead of `u3`.

### Semantic dedupe of paraphrased duplicates

```yaml
vector:
  action: rank
  output: deduped
  threshold: 0.85
  items:
    - {id: "a", text: "OpenAI Sora 2 launches today"}
    - {id: "b", text: "Sora 2 hits public beta — what's new"}
    - {id: "c", text: "Apple ships Vision Pro 2"}
```

Returns 2 items: the Sora paraphrases collapse into one.

### Index + search (persistent retrieval)

```yaml
# Write-time
vector:
  action: index
  namespace: "catchup_2026q2"
  items:
    - {id: "u1", text: "OpenAI Sora 2 launches", metadata: {url: "...", captured_at: "..."}}
    - {id: "u2", text: "DeepMind Gemini 3 preview", metadata: {url: "..."}}

# Later — read-time
vector:
  action: search
  namespace: "catchup_2026q2"
  query: "multimodal AI"
  mode: hybrid
  limit: 5
```

## Failure modes

| Symptom | Cause | What the agent sees |
|---|---|---|
| Tool missing from catalog | Ollama not reachable / model not pulled | `vector` simply isn't in the tool list. Fall back to whatever non-semantic path the workflow has. |
| `{status: "ollama_unavailable", ...}` returned at execute time | Ollama died between catalog build and dispatch | Per-call defense-in-depth: agent gets a clean error envelope instead of a crash. |
| `vector.index failed: Ollama returned ...` | Embedding pipeline error (model misconfigured, network) | Real error; investigate Ollama logs. |
| `vector.search` returns empty | Namespace doesn't exist or has no items | Agent should check `vector.index` was called for this namespace first. |

## Key files

- `magician-vector-index/src/vector_toolkit.rs` — `OllamaEmbedder`, `VectorTable`, `RankOutput`, `SearchMode` — the embedder + lancedb-backed primitives.
- `magician/src/magician_v2/execution/compiled_providers.rs::VectorCapabilityProvider` — provider impl.
- `magician/src/magician_v2/execution/embedded_pack_defs/vector.yaml` — agent-facing pack definition.
- `magician/src/magician_v2/runtime/ollama_lifecycle.rs` — boot/shutdown/health-probe.
- `magician/src/magician_v2/execution/agentic/native_catalog.rs` — catalog filter (drops `vector` when unavailable).

## Sibling capability: `catchup_merge`

The `catchup_merge` compiled provider is a sibling of `vector` — both run
100% in-process and both reuse `magician_vector_index::normalize_url` as
their single source of truth for URL canonicalization. They're independent:

- `catchup_merge` does deterministic cross-source merge (URL dedup, RRF,
  engagement+freshness scoring, Jaccard clustering). No embeddings, no
  Ollama dependency.
- `vector` does semantic ops (embed, search, rank). Requires Ollama.

Typical composition: `catchup_merge` first (deterministic dedup + ranking),
then `vector.rank(output=ranked, query=topic)` on the merged items
(semantic rerank against the user's query). Each is useful standalone.

See `magician/src/magician_v2/execution/embedded_pack_defs/catchup_merge.yaml`.

## See also

- Catch-up Research Stack — primary consumer of vector ops for semantic rerank + cross-session retrieval.
- [Memory Index](memory-index.md) — the agent memory tier uses the same lancedb primitives via a parallel path (`memory_index.rs`).
- [Skills Authoring](skills-authoring.md) — how to add new agent capabilities.
