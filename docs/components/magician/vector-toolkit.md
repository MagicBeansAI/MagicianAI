# Vector Toolkit

Owner: magician runtime.

## What it is

A generic semantic-vector capability exposed to agents as the `vector` tool. Three sub-actions backed by lancedb + Ollama embeddings:

| Action | Purpose | Persistence |
|---|---|---|
| `vector.index` | Embed + persist items to a named namespace (lancedb table). Upsert-by-id. | Yes — `<scope>/vector_tables/<namespace>/` |
| `vector.search` | Query a persisted namespace by hybrid FTS + vector / FTS-only / vector-only. | Reads persisted state. |
| `vector.rank` | Reorder / cluster / dedupe a flat list of items in-memory. | None — stateless. |

Built on the same lancedb infrastructure as the agent memory index (`magician-vector-index` crate), in-process Rust with no subprocess.

Exact prompt-time query embeddings are coalesced across concurrent retrieval
branches, keyed by endpoint, model, dimensions, context, batch-token contract and
exact query. Successful vectors stay reusable for a 25 ms sibling window;
failures are evicted immediately. Document/index embeddings use read-first
admission and yieldable write batches.

## Why a generic toolkit (not a per-domain provider)

Embedding, hybrid retrieval, clustering and dedup recur across research, RAG and
topic-detection workflows (catch-up rerank, clustering error reports, feedback
themes). One three-action capability serves all of them, so no per-workflow
provider exists for any of these.

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

`implementation: { type: compiled, provider_name: "vector" }`, like `shell`, `http`, `files`, `task_state`: a single-call utility with no LLM round-trip. See `magician/src/magician_v2/execution/compiled_providers.rs::VectorCapabilityProvider`.

### Storage layout

```
<scope>/vector_tables/
  <namespace>/            ← lancedb table dir; one per namespace
    items/                ← lancedb internals (Arrow batches, FTS index)
    _versions/
    ...
```

Per-table schema: `(id: utf8, text: utf8, metadata: utf8 [JSON-stringified], embedding: fixed_size_list<f32, dims>)` + FTS index on `text`.

`namespace` is agent-chosen: `task_<task_id>` for ephemeral per-research pools, stable names for long-lived caches.

**Security — namespace allowlist:** the namespace is a path segment, so
`is_safe_namespace` (checked before every action; an omitted namespace is
`default`) rejects empty, leading-dot and `..`-containing names and any character outside
`[A-Za-z0-9_.:-]`.

**Index semantics — merge-only at runtime:** the first `vector.index` creates the
table and FTS index; later calls `merge_insert` by `id`. Runtime indexing never
deletes, replaces or optimizes the namespace. Duplicate IDs within one request
are rejected. `VectorTable` also has non-creating existence checks and exact-ID
deletion for owned derived-index maintainers (e.g. procedure indexing); these
are not agent-facing.

**Threshold validation:** `vector.rank` requires `threshold ∈ [0.0, 1.0]`
(cosine on L2-normalized vectors); out of range is an error. Default 0.85 for
`clusters`, 0.90 for `deduped`.

## Ollama lifecycle

The toolkit needs a running Ollama daemon with the configured embedding model
pulled. Magician owns the lifecycle.

### On boot (`magician/src/magician_v2/runtime/ollama_lifecycle.rs::start`)

1. Resolve the dedicated embedding endpoint from
   `runtime.ollama.embedding_base_url` (default `127.0.0.1:11435`).
2. Probe the base URL (`GET /api/tags`). If reachable → use the existing
   daemon (do not spawn or stop it).
3. If a local endpoint is not reachable and `MAGICIAN_OLLAMA_AUTOSTART` is not
   false, locate `ollama` on PATH (or `/opt/homebrew/bin/ollama`,
   `/usr/local/bin/ollama`, `/usr/bin/ollama`) and spawn `ollama serve` with
   the configured admission capacity (`OLLAMA_NUM_PARALLEL`, one verified
   physical sequence) and one loaded-model slot, then wait up to 15 s for it
   to answer. Remote endpoints are never spawned.
4. Check the embedding model is installed, then prewarm it with
   `keep_alive: -1`. If it is missing or residency fails, the capability stays
   unavailable and the configuration error is reported. Runtime never pulls or
   substitutes a model; `make setup-ollama` installs it.

`make run-supervisor` runs the shell lifecycle first: it records its PID and
launch signature, restarts mismatched owned daemons, and can replace a
conflicting local daemon. Direct `magician.bin` launches use only the in-process
behavior above.

### While running

A background task polls `/api/tags` every 30s; recovery prewarms the model
before marking the capability available. Retrieval embeddings use the
foreground lane. Background and durable-write requests are small and release
admission after every provider call, so a queued foreground read goes first.
The verified capacity must be one; configuration fails closed rather than assume
parallel embedding sequences. While unavailable, `vector` is hidden from new
agent catalogs.

### On shutdown (`stop`)

- If magician spawned the daemon → SIGTERM, wait up to 5s, SIGKILL.
- If pre-existing → leave alone.
- `make stop-supervisor` / `make stop-all` also run `scripts/stop-ollama.sh`,
  which unloads resident models before teardown, releasing memory even when the
  daemon is user-managed.

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

Generation stays on the `:11434` daemon; the shell lifecycle
(`scripts/run-ollama-embedding.sh`) manages a second PID/signature/log for the
embedding-only `:11435` daemon and exits non-zero on a missing binary, an
unreachable remote endpoint, a non-Ollama listener it may not replace, or a
failed prewarm/residency check (model missing, or not resident within 10 s),
rather than falling back.

The embedding contract is config-only: `runtime.ollama.embedding_model`,
`embedding_dimensions`, `embedding_context_tokens`, `embedding_batch_tokens`,
`embedding_batch_size`, `embedding_num_parallel` and
`embedding_max_loaded_models`, so the launcher and in-process admission agree on
capacity. Endpoint, residency, timeouts and cache/flash tuning are config-first
with the emergency env overrides above.

`embedding_context_tokens` is the model-context boundary; `embedding_batch_tokens`
is the runner evaluation batch and the largest physical prompt Ollama's
llama.cpp embedding path accepts. Text over the smaller ceiling is split at
UTF-8-safe semantic boundaries, embedded as fragments, and pooled into one
length-weighted normalized vector. Both ceilings are part of the persisted
embedding contract, since fragmentation changes the pooled vector. Every request
disables Ollama's silent truncation; bounded HTTP error details keep provider
causes such as physical-batch overflow. Response count, dimensions and finite
values are validated. This applies to vector tools, memory documents and
oversized retrieval queries; callers get exactly one vector per logical input.

Request-path query embeddings may reuse an exact contract-plus-query vector from
a process-wide LRU (`runtime.retrieval.query_vector_cache`, Magician default on)
and may share one `/api/embed` call across distinct same-contract queries already
queued (`runtime.ollama.embedding_query_batch_*`, default window 3 ms, max 8
items). An empty queue dispatches at once; with two or more queued, the leader
waits at most one window, then flushes. Kill switches:
`MAGICIAN_QUERY_VECTOR_CACHE=off`, `MAGICIAN_EMBEDDING_QUERY_BATCH=off`. The
crate `Default` for these is off until Magician installs YAML.

`SearchMode::Vector` stays exhaustive nearest-neighbor on the tool table.
Toolkit tables carry only an FTS index. IVF_PQ / ANN search is a
memory-hybrid concern (`runtime.retrieval.vector_search`: Magician default
`ann`, crate default `flat`) and does not affect this toolkit.

URL and timeout knobs keep their `MAGICIAN_MEMORY_*` aliases. Each
`OllamaEmbedder` reuses one `reqwest::Client` for embed, health and tag calls;
embedding budgets cover scheduler admission plus remaining HTTP time, while
health probes keep their own timeout.

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
| Tool missing from catalog | Ollama not reachable / model not pulled | `vector` is not in the tool list; fall back to a non-semantic path. |
| `{status: "ollama_unavailable", ...}` returned at execute time | Ollama died between catalog build and dispatch | A clean error envelope instead of a crash. |
| `vector.index failed: Ollama returned ...` | Embedding pipeline error (model misconfigured, network) | Real error; check Ollama logs. |
| `vector.search` returns empty | Namespace missing or empty | Check `vector.index` ran for this namespace. |

## Key files

- `magician-vector-index/src/vector_toolkit.rs` — `OllamaEmbedder`, `VectorTable`, `RankOutput`, `SearchMode`.
- `magician/src/magician_v2/execution/compiled_providers.rs::VectorCapabilityProvider` — provider impl.
- `magician/src/magician_v2/execution/embedded_pack_defs/vector.yaml` — agent-facing pack definition.
- `magician/src/magician_v2/runtime/ollama_lifecycle.rs` — boot/shutdown/health-probe.
- `magician/src/magician_v2/execution/agentic/native_catalog.rs` — catalog filter (drops `vector` when unavailable).

## Sibling capability: `catchup_merge`

`catchup_merge` is an independent in-process compiled provider; both it and
`vector` use `magician_vector_index::normalize_url` for URL canonicalization.

- `catchup_merge` does deterministic cross-source merge (URL dedup, RRF,
  engagement+freshness scoring, Jaccard clustering). No embeddings, no Ollama.
- `vector` does semantic ops (embed, search, rank). Requires Ollama.

Typical composition: `catchup_merge` first, then
`vector.rank(output=ranked, query=topic)` on the merged items. See
`magician/src/magician_v2/execution/embedded_pack_defs/catchup_merge.yaml`.

## See also

- Catch-up Research Stack — primary consumer of vector ops for semantic rerank + cross-session retrieval.
- [Memory Index](memory-index.md) — the agent memory tier uses the same lancedb primitives via a parallel path (`memory_index.rs`).
- [Skills Authoring](skills-authoring.md) — how to add new agent capabilities.
