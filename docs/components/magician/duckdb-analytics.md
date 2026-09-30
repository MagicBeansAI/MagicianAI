# DuckDB Analytics Architecture

## Purpose

DuckDB serves two isolated roles in the Magician runtime:

- a persistent internal analytics database for Magician system events
- a separate agent-facing query capability for analytical work

Internal telemetry and ad hoc agent queries do not share state or lifecycle.

The agent-facing operating guide for the `duckdb` capability is the inline
`guide:` field of
`magician/src/magician_v2/execution/embedded_pack_defs/duckdb.yaml`, compiled
in via `include_str!` in `execution/compiled_providers.rs`. The inner loop
reads `pack.guide.as_deref()` and injects it into the LLM context when DuckDB
is invoked. Editing the guide requires a rebuild, the same constraint as the
other embedded pack defs.

## Internal Analytics Database

The internal analytics layer writes to scoped DuckDB files at
`magician_data_v3/scopes/<principal>/<workspace>/analytics/analytics.duckdb`
through a system-owned pool. It stores schemaless events in a single `events`
table and projects read views on top of that base table.

Current characteristics:

- scoped file-backed DuckDB pools, materialized lazily on first analytics event
  or analytics API query. `AnalyticsDispatcher` owns one shared read-write
  `DuckDbPool` per `(principal, workspace)`; query/read paths use a coexisting
  `read_connection()`. Opening a second read-write handle races the file lock.
- the Magician library, API, and CLI binaries statically link the pinned DuckDB
  JSON and Parquet extensions (`duckdb = "=1.10502.0"` with `bundled`, `json`,
  `parquet` in `magician/Cargo.toml`). Production startup and analytics must
  not depend on DuckDB extension autoloading, a user extension cache, or
  network installs. Query paths then set `enable_external_access`,
  `autoinstall_known_extensions`, and `autoload_known_extensions` to false
  after installing server-owned views.
- global `analytics::emit()` fire-and-forget write path (`analytics/mod.rs`).
  No-op when the dispatcher was never initialized.
- batched event sink (`event_sink.rs`): channel capacity 10_000, flush every
  100 events or 500 ms. Drops with a counter when the channel is full.
- every batch is dual-written to a version-stable Parquet mirror at
  `<scope>/analytics/events/dt=YYYY-MM-DD/batch_<ulid>.parquet`. An
  `analytics.duckdb` this binary cannot read is renamed
  `analytics.duckdb.incompatible-<ts>` (preserved, never deleted), recreated
  fresh, and rebuilt from that mirror. Lock conflicts are not treated as
  corruption.
- event-table retention keeps all rows newer than 24 hours and always keeps
  the newest 1000 rows per `event_type`, whichever preserves more rows. Sweep
  cadence is 5 minutes.
- schema catalog generation persisted to
  `<scope>/analytics/schema_catalog.json`
- read-only query API at `/api/magician/v2/analytics/schema` and
  `/api/magician/v2/analytics/query` (handlers in
  `magician-api/src/analytics_api.rs`; routes registered on the v2 scope in
  `magician-bin/src/main.rs`). Missing `AnalyticsApi` returns 503.

Caller-supplied SQL is parsed through `parse_analytics_sql`
(`analytics/llm_sql_guard.rs`), which bounds nesting depth at
`ANALYTICS_SQL_RECURSION_LIMIT` (20). `sqlparser` is recursive descent over a
large AST and its own default depth of 50 does not fit an actix worker's
2 MiB stack — a deeply nested query overflows and aborts the *process* before
the parser's own limit is ever reached. Every analytics parse site must use
this helper rather than `Parser::parse_sql`.

## Event Model

Analytics is event-based rather than table-per-domain. Typed constructors on
`AnalyticsEvent` (`event_sink.rs`) cover `log`, `bot_log`, `chat_session`,
`chat_message`, `task_execution`, `task_step`, `artifact_registered`, and
`artifact_transition`. Coding-run events (`coding.started` / `completed` /
`stats` / `failed`) use the same `analytics::emit()` path. Payload schema
can evolve without table migrations.

## Convenience Views

`views.rs` creates derived views over `events` (idempotent `CREATE OR REPLACE`):
`logs`, `bot_logs`, `chat_messages`, `chat_sessions`, `task_executions`,
`task_steps`, `artifacts`. Best-effort memory views (`episodes`,
`memory_tiers`) scan existing JSON on disk via `read_json_auto()` and are
skipped if the directory tree is absent.

The compiled `internal_data` pack attaches the analytics database read-only
for investigation work (`query_events`, `tail_logs`, `find_errors`).

## Agent-Facing DuckDB Capability

The `duckdb` pack is a separate execution surface:

- `implementation.type: primitive`, `provider_name: duckdb`
- session-scoped in-memory DuckDB connection by default
  (`DuckDbSession` in `execution/native_executors.rs`)
- optional persistent `.duckdb` file when the `database` parameter is set
- intended for data analysis agents and operator workflows

This capability is not the internal analytics pool, even though both use
DuckDB. In-memory tables and views persist across actions in the same
session; file-backed mode opens a fresh connection per action, so temp
tables do not survive.

### Inner-loop actions

Seven typed inner-loop actions in `duckdb.yaml` all route through
`DuckDbCapabilityProvider::lower` → `lowering::lower_duckdb_action`.
Inner-loop dispatch is `CompiledProviderDispatcher`
(`execution/primitive_dispatch/compiled_provider.rs`), which injects
`__action_name` (alongside `__principal` / `__workspace` / `__task_id`).
Direct-dispatch callers without `__action_name` default to `query`.

- `query` — full SQL. Default action.
- `preview` — `SELECT * FROM '<source>' LIMIT <n>`. Escapes quotes; clamps
  non-positive `limit` to 1.
- `describe` — `DESCRIBE SELECT * FROM '<path>'`, or `DESCRIBE <table>` when
  `is_table=true`. Identifiers must match `[A-Za-z0-9_]`.
- `list_tables` — `SHOW TABLES`. Default `output_format` is `table`.
- `read_parquet` — `SELECT <select> FROM read_parquet('<path>', hive_partitioning = <bool>)`
  with optional `WHERE`/`LIMIT`. `select` defaults to `*`,
  `hive_partitioning` to `true`. `path` accepts
  `llm_calls@<principal>/<workspace>`, expanded to
  `magician_data_v3/scopes/<principal>/<workspace>/analytics/llm_calls/dt=*/*.parquet`.
- `export` — `COPY (<sql>) TO '<output_path>' (FORMAT '<format>'[, HEADER true])`.
  Whitelists `csv` / `parquet` / `json`. `HEADER true` only for CSV when
  `header=true` (default).
- `attach` — `ATTACH '<attach_path>' AS <alias> [(READ_ONLY)]`. Alias must
  match `[A-Za-z0-9_]`. Empty path/alias rejected. `read_only` defaults to
  `true`. Named `attach_path` so it does not collide with session `database`.

## Operational Boundaries

- Analytics is optional at the call site: `analytics::emit()` is a no-op if
  the dispatcher was never initialized, and analytics HTTP handlers return
  503 when `AnalyticsApi` is absent. Pool open failures for a scope are
  logged and skip that write rather than aborting the process.
- The analytics database is system-owned. Agents query it read-only through
  `internal_data` or the analytics HTTP surface.
- Agent-created temporary tables and notebook workflows do not mutate the
  internal analytics connection.

## LLM Calls Parquet Lakehouse

A separate, append-only telemetry surface lives alongside scoped
`analytics.duckdb` event stores and is intentionally isolated from them.

Two writers share the same `LLMResponseReceived` broadcast:

- **Canonical facts** — `LlmTraceActivation` (journal + materializer) is
  process-owned and active by default. Governed reads go through
  `LlmAnalyticsReadService` (`/api/magician/v2/analytics/llm/*` and
  `internal_data` typed actions / `query_llm_facts`).
- **Compatibility mirror** — `LlmParquetSink`
  (`analytics/llm_parquet_sink.rs`) still writes date-partitioned
  `batch_<ulid>.parquet` files under
  `<scope>/analytics/llm_calls/dt=YYYY-MM-DD/` so `/llm` and
  `POST /api/magician/v2/analytics/llm_calls/query` keep working. The
  runtime does no aggregation — slice/dice happens in SQL at query time.

The Parquet files are the cold store for the compatibility path. Schema
lives in the files; DuckDB handles evolution at read time via
`union_by_name`. Retention is a directory delete, not DDL.

### Write path (compatibility mirror)

```
LLM call ──▶ emit RuntimeTransportEvent::LLMResponseReceived
              │  (chat, agentic decide, operation router, dispatch seam,
              │   mail/channel helpers, voice orchestrator)
              ▼
       RuntimeTransportBroadcaster
              │
              ├── SSE (live UI)
              ├── events.jsonl (per-execution)
              ├── transport_log.jsonl (per-scope)
              ├── LlmTraceActivation (canonical journal)
              └── LlmParquetSink (compatibility mirror)
```

`LlmParquetSink` is a tokio task that subscribes to the broadcaster,
filters to `LLMResponseReceived`, and buffers rows per
`(principal, workspace)`. Flush triggers: 30s wall-clock, 100 rows in any
scope's buffer, or shutdown. Each flush opens an in-memory DuckDB
connection, builds a temp table via the appender API, then runs
`COPY ... TO '<scope>/analytics/llm_calls/dt=YYYY-MM-DD/batch_<ulid>.parquet'
(FORMAT PARQUET, COMPRESSION 'zstd')`. Date partition is derived from the
first row's `timestamp_ms` at flush time. `Lagged` subscriber events log a
"broadcaster lagged; dropped N events" warning and skip-and-continue. Writes
use the shared analytics DuckDB guard and the checked connection setup
(`threads = 1`).

Chat-inline calls emit the same canonical `LLMResponseReceived` rows as
autonomous executions. Personal Tutor and Live Concept Tutor ride the
chat-inline path. Overlay-only tools such as `screen-draw` do not create
LLM rows because they do not call a model.

### Pricing

`cost_usd` is computed at emit time via `magicllm::compute_cost_at`
against the active effective-dated pricing table: built-in launch/base
rates plus any built-in provider revisions compiled into `magicllm`, with
the deployment pricing file layered on top. The file lives at
`<MAGICIAN_ROOT_DIR>/llm_pricing.json`; its git-tracked seed template is
`magician_data_v3/llm_pricing.template.json` (loaded directly by dev
checkouts without a runtime copy via `runtime_config_path`).
`magician-bin/src/main.rs` calls
`llm_pricing_config::load_and_install_llm_pricing()` once, immediately
after main config load — before any service construction or CLI command
can price a call, because the pricing `OnceLock` silently locks in the
builtin-only table on first use. Load is fail-open: a missing or invalid
file logs one warning and leaves the built-in table active.

Resolution for a call at time `T` considers only rows already effective at
`T`, picks the longest `model_prefix` match, then the latest
`effective_from` among equal-length prefixes — so a price change is a new
dated row in the file, never an edit. Priced call sites pass the call's
start timestamp (`started_at_ms`). Stored rows remain frozen at write:
editing `llm_pricing.json` affects new calls only.

`prompt_tokens` is the complete input count and already includes cache-read
and cache-creation tokens. The calculator subtracts those cache buckets
before billing uncached input, then bills each cache bucket exactly once
at its own rate (or at `input_per_m` when that optional bucket rate is
absent). A row may also carry a `long_context` object; when
`prompt_tokens` is strictly greater than `threshold_tokens`, its input
multiplier applies to uncached and cached input and its output multiplier
applies to output. A row may also model an explicit cache-write bucket.
Model rates themselves live in the seed template, not here.

Corrective repricing: `magician analytics reprice-llm-calls`
(`make analytics-reprice-llm`) recomputes each stored row's `cost_usd` at
the row's own `timestamp_ms` and atomically rewrites only files whose
costs changed. Dry run unless `--apply`. Scope with
`--principal`/`--workspace` and inclusive `--from`/`--to`. Skips today's
live partition unless `--include-today`. Blank provider/model rows are
counted as skipped. Publish path: `COPY` into a UUID staging sibling,
`fsync`, row-count check, rename, `fsync` the partition directory. The
sweep does not lock the file, so a writer that appends between read and
rename loses those rows.

#### Operator runbook: updating prices

One-time setup: `cp magician_data_v3/llm_pricing.template.json
<MAGICIAN_ROOT_DIR>/llm_pricing.json` (default root `~/MagicianNotes`).
The runtime copy wins and is never overwritten by updates. The file is
read once at startup.

**Price revision.** ADD a new row with the revision date — never edit the
old row. Late revisions: dry-run then
`magician analytics reprice-llm-calls --from <date> --apply`.

**New model.** Add a row whose `model_prefix` matches the model id, dated
from first use. A model with no matching row prices to **$0 silently**;
recover with the reprice command.

**New provider.** Use the provider string exactly as it appears in the
magicllm provider config. Empty `model_prefix` is the per-provider
catch-all; absent cache fields bill cached tokens at the input rate.

### Compatibility schema

One row per LLM call on the `llm_calls` compatibility relation
(`legacy_llm_compat.rs`). Defaults apply only when a column is physically
absent from the Parquet; a stored SQL `NULL` remains unknown. Historical
`reasoning_summary` is omitted. A stored free-form `error` is reduced to
the presence marker `legacy_error_redacted`. Non-finite `cost_usd` (legacy
NaN rows) projects as SQL `NULL` so a single poisoned value cannot blank
an aggregate. `response_kind` that fails a conservative charset is
replaced with `invalid_category_redacted`.

| Column | Type | Notes |
|---|---|---|
| `timestamp_ms`, `started_at_ms`, `latency_ms`, `ttft_ms` | BIGINT | End, start, duration, optional TTFT |
| `principal`, `workspace` | VARCHAR | Scope |
| `schema_version` | INTEGER | |
| `trace_id`, `llm_call_id`, `provider_attempt_id`, `dispatch_job_id`, `parent_call_id`, `parent_relation`, `retry_group_id`, `route_decision_id` | VARCHAR | Lineage |
| `scope_resolution` | VARCHAR | Default `'legacy_default'` when absent |
| `root_execution_id`, `iteration_id`, `prompt_projection_mode`, `chat_turn_id` | VARCHAR | |
| `workload_class` | VARCHAR | Default `'system'` when absent |
| `call_role` | VARCHAR | Default `'primary'` |
| `provider_attempt_count` | INTEGER | `0` is a bookkeeping aggregate, not a model call |
| `response_reused` | BOOLEAN | |
| `execution_id`, `task_id`, `plan_id`, `step_id`, `agent_id`, `delegated_agent_id`, `chat_session_id` | VARCHAR | nullable except `execution_id` may be present |
| `step_index` | BIGINT | nullable |
| `operation`, `profile`, `provider`, `model`, `capability` | VARCHAR | |
| `response_kind` | VARCHAR | `tool_call` / `text` / `streaming` / `reasoning_only` / `error` |
| `attempt` | INTEGER | 1-based; > 1 means retry |
| `success` | BOOLEAN | |
| `error` | VARCHAR | redacted presence marker or NULL |
| `input_tokens`, `output_tokens`, `reasoning_tokens`, `cache_read_tokens`, `cache_creation_tokens` | BIGINT | |
| `cost_usd` | DOUBLE | |
| `dt` | VARCHAR | hive partition date when present |

The on-disk compatibility batch also stores audio token splits; those are
not projected on the caller-visible `llm_calls` view.

### Emit-site coverage

`LLMResponseReceived` is the shared emit event. The parquet sink and the
canonical journal both subscribe to it.

**Autonomous path** (`execution/agentic/run_loop/phases/decide.rs`) —
emits for every decision call. Telemetry originates in
`execution/agentic/native_integration.rs`: the router's
`dispatch_execution_native_messages` stamps the effective provider/model
onto `ExecutionNativeRouterResponse`, the native adapter threads them onto
`ExecutionNativeResponse`, and `cost_usd` is computed via
`magicllm::compute_cost_at` with `started_at_ms` stamped at telemetry
construction.

**Chat-inline path** (`chat/service.rs::process_chat_inline_turn`) —
emits for every LLM call inside a chat turn, success and failure.
Successful rows resolve the selected profile into provider/model metadata,
preserve provider token/cache/reasoning usage when returned, and compute
`cost_usd` through `magicllm::compute_cost_at`. Filter
`capability = 'chat.inline'` to isolate chat-mode usage.

**Scoped direct-operation path** (`analytics/operation_llm_telemetry.rs`
and `query_analysis/operation_llm_router.rs`) — covers operation-router
calls that run outside chat and the agentic decision loop. The bridge
requires an explicit principal/workspace and preserves provider, model,
profile, token usage, cache/reasoning usage, call start time, and priced
cost from the router response. It emits no row when a response has no
router telemetry. Parent-owned chat and agentic calls do not use this
bridge, avoiding double-counting. Channel/mail helpers emit through
`llm_dispatch_seam.rs`; voice realtime calls emit from
`magician-media` `voice_orchestrator.rs`.

Provider-native TTS/STT/VAD/diarization calls remain on their media
telemetry surfaces unless their provider exposes billable usage.

### Query path

All of these live under `/api/magician/v2` (not v3):

```text
POST /api/magician/v2/analytics/llm_calls/query
POST /api/magician/v2/analytics/llm_calls/query_batch
GET  /api/magician/v2/analytics/llm/overview
GET  /api/magician/v2/analytics/llm/catalog
GET  /api/magician/v2/analytics/llm/schema
GET  /api/magician/v2/analytics/llm/calls[/{llm_call_id}]
GET  /api/magician/v2/analytics/llm/traces[/{trace_id}]
GET  /api/magician/v2/analytics/llm/provider-attempts[/{id}]
POST /api/magician/v2/analytics/llm/facts/query
```

Handlers are in `magician-api/src/analytics_api.rs`. Routes are registered
in `magician-bin/src/main.rs` on `web::scope("/api/magician/v2")`.

`POST .../llm_calls/query` accepts `{ "sql": "..." }`, opens a fresh
in-memory DuckDB, and installs a compatibility projection over
server-selected files scoped to the caller. The raw Parquet scan is never
a caller-visible relation. `error` is projected as NULL or
`legacy_error_redacted`.

#### One row per call

The governed observability pipeline also writes call-level `call_fact`
lifecycle records (record_revision 1 `started`, 2 `completed`) into this
same `llm_calls` dataset; those records carry NULL provider/model by
design (provider/model live on the per-attempt `llm_provider_attempts`
records), and would otherwise show as an empty provider/model group. The
compatibility relation therefore projects
**exactly one row per call** (`legacy_llm_compat::build_llm_calls_relation`):

- prefer the legacy flat `batch_*` fact row (already carries
  provider/model/cost);
- otherwise the completed `call_fact` (record_revision = 2), with
  provider/model reconstructed from its winning provider attempt
  (succeeded first, else the last attempt) read from the sibling
  `analytics/llm_provider_attempts` dataset.

The `started` lifecycle rows and the duplicate completed rows for calls
the legacy sink already recorded are dropped. A pure legacy lake (no
`call_fact`, no `record_kind` column) is read unchanged.

The SQL parser requires exactly one `SELECT`/`WITH` statement and walks
its AST. Only `llm_calls`, `chat_session_cache_summary`,
`llm_embeddings`, and lexically in-scope CTEs are accepted
(`legacy_llm_compat::ALLOWED_RELATIONS`). Qualified/catalog relations,
relation shadowing, table functions and external scans are rejected. A
shared structural guard also rejects `SELECT INTO`, `VALUES`, `TABLE`
and mutation-shaped query bodies. After the server-owned views are
installed, DuckDB external access is disabled. The structural check
applies recursively to CTEs, derived tables and expression subqueries.

Result materialization is hard-capped at 10_000 rows and 4 MiB
(`ANALYTICS_DUCKDB_MAX_RESULT_ROWS` /
`ANALYTICS_DUCKDB_MAX_RESULT_BYTES`). Single responses and a batch's
complete result array share that serialized ceiling. Decode errors fail
closed. Response shape: `{ "columns": [...], "rows": [[...]], "row_count": N }`.

Handlers share the analytics DuckDB guard and conservative connection
setup (one execution thread, 512 MB memory, 1 GB temp). Concurrent `/llm`
and `/memory` cards therefore queue. `query_batch` (max 8 SQL statements,
5s guard wait, 8s per item) grabs the guard once.

The compiled internal analyst uses governed typed reads and
`query_llm_facts` by default, and is instructed not to call
`duckdb.read_parquet` against historical `analytics/llm_calls`. The same
query-shape guard covers generic/memory analytics SQL and compiled
internal event/memory query actions.

### Retention and lifecycle

Lakehouse retention is **not** a per-sink cron and there is no
`analytics.llm_calls_retention_days` config key (`LlmParquetRetention` in
`llm_parquet_sink.rs` is unused and never spawned). The singular owner is
`StorageMaintenanceRuntime`
(`analytics/parquet_maintenance.rs`), spawned from
`magician-bin/src/main.rs` after the sinks. It coordinates rolling
canonical LLM compaction, completed-day batch compaction, and the shared
90-day events / memory / LLM lineage retention policy
(`DEFAULT_ANALYTICS_RETENTION_DAYS = 90`, full sweep every 6 hours).
Keeping this singular avoids a legacy retention worker racing verified
Parquet publication. Catalog entries live in storage governance
([storage-governance.md](storage-governance.md)): `llm_calls`,
`llm_embeddings`, `llm_dispatch`, `llm_provider_attempts`,
`llm_tool_calls`, `llm_capture_gaps`, `events`, and `memory_events` are
90-day observability streams.

On shutdown, `magician-bin` awaits the canonical LLM drain, then
`LlmParquetSink::shutdown` (final compatibility flush), then the
embeddings sink, then cancels analytics and awaits the activity
forwarder before draining the spine sink.

### Boundary vs scoped `analytics.duckdb`

- Scoped `analytics.duckdb` files are for general operational events.
  They use persistent file-backed pools and are bounded by per-event-type
  retention, with a Parquet mirror for format recovery.
- The LLM Calls lakehouse is for LLM telemetry. Each compatibility query
  opens a fresh in-memory DuckDB; the cold store is the Parquet files;
  there is no shared schema with `analytics.duckdb`.
- Canonical LLM facts are a third surface (journal-backed, governed
  reads). They do not share pools with either of the above.

## LLM Embeddings Parquet Lakehouse

Local embedding calls (the on-device Ollama embedder used by memory
indexing, resurfacing centrality, procedure indexing, and attention
learning) are captured into a **separate** flat dataset — deliberately
not tagged rows in `llm_calls` — so the hot per-call query path is never
bloated by high-frequency, zero-cost embedding telemetry.

### Write path

One flat, content-free record is emitted per embed **batch** by
`LlmEmbeddingsSink` (`analytics/llm_embeddings_sink.rs`): a
fire-and-forget `try_record` submit (drop-with-counter on backpressure —
never blocks or panics the caller; queue capacity 512), a bounded flush
channel, and a DuckDB appender that `COPY ... TO`s zstd Parquet at:

```text
<scope>/analytics/llm_embeddings/dt=YYYY-MM-DD/embed_<ulid>.parquet
```

The scope root is
`ArtifactV2Workspace::analytics_llm_embeddings_root(principal, workspace)`.
Buffers flush every 30s, at 100 records, or on shutdown. On shutdown the
flush channel's backlog is drained into the per-scope buffers *before*
the final flush. Completed-day compaction and retention are owned by
`StorageMaintenanceRuntime`, not the sink. The sink is registered once
at startup as a process-global (`set_global_sink`). Call sites record via
`record_embedding_batch(...)`, a no-op when no sink is registered.

The `OllamaEmbedder` lives in `magician-vector-index` and has no scope
context, so recording happens at magician-side callers.

Wired operations:

- `resurfacing` —
  `attention/resurfacing/centrality.rs::EmbeddingCentrality::build`.
  One record per reference batch; `input_tokens` is the `len/4` estimate.
- `memory_index` — `analytics/memory_index_maintainer.rs` after each
  successful incremental update / reconcile. `batch_size` =
  `MemoryLanceDbWriteReport.embedded_rows` (falls back to `chunk_count`).
  Records only when `batch_size > 0`. Embedded texts are not in hand, so
  `input_tokens = 0`.
- `procedure_index` —
  `learning/procedure_index.rs::refresh_procedure_index` after
  `VectorTable::index` on the changed set.
- `attention_learning` — `attention/learning/mod.rs` records each
  on-demand candidate embed.

`elicitation` is **not** an embedding site:
`elicitation/manager.rs::embed_metadata` embeds JSON metadata into a
schema object and performs no vector embedding, so it is not recorded.

### Schema

| column | type | meaning |
| --- | --- | --- |
| `timestamp_ms` | BIGINT | batch completion time (epoch ms) |
| `principal` / `workspace` | VARCHAR | scope |
| `provider` | VARCHAR | `ollama` |
| `model` | VARCHAR | configured embed model |
| `operation` | VARCHAR | purpose tag |
| `input_tokens` | BIGINT | Ollama-reported count if available, else `sum(len/4)` |
| `batch_size` | INTEGER | number of inputs embedded |
| `cost_usd` | DOUBLE | always `0.0` |
| `latency_ms` | BIGINT | measured wall-clock for the batch |
| `success` | BOOLEAN | whether the embed batch succeeded |

### Query path

```text
POST /api/magician/v2/analytics/llm_embeddings/query
```

Handler `query_llm_embeddings_handler` mirrors `query_llm_calls_handler`:
install a flat content-free `llm_embeddings` TEMP TABLE over
`read_parquet(..., hive_partitioning = true, union_by_name = true)` with
typed defaults (no reconciliation), then run the caller's SELECT under
the same guards. `llm_embeddings` is in `ALLOWED_RELATIONS`.

## Memory Events Parquet Lakehouse

```text
magician_data_v3/scopes/<principal>/<workspace>/analytics/memory_events/dt=YYYY-MM-DD/*.parquet
```

The runtime writes rows from three high-value paths:

- prompt memory retrieval (`event_kind = 'retrieval'`)
- memory consolidation transforms (`event_kind = 'consolidation_transform'`)
- memory retrieval evaluation cases (`event_kind = 'eval_case'`)

Retrieval rows capture the query excerpt, agent/goal, scope, tier, item
key, score, confidence, whether the item was injected, candidate counts,
dropped counts, budget limits, and output size. Consolidation rows
capture agent, rule, target, source kind, input/emitted/skipped counts,
status, and a compact JSON payload. Eval rows capture suite/case ID,
query, pass/fail, expected/matched counts, best matched rank, status,
and optional JSON details.

Writes are buffered in-process per scoped analytics root and flush every
5 seconds or 100 rows (`memory_parquet.rs`). Each flush writes one
date-partitioned Parquet batch via DuckDB `COPY`, guarded by the shared
analytics DuckDB lock and the checked connection setup (`threads = 1`).

Query endpoints:

```text
POST /api/magician/v2/analytics/memory_events/query
POST /api/magician/v2/analytics/memory_events/query_batch
```

The endpoint creates a scoped DuckDB view named `memory_events` over the
Parquet glob with `union_by_name = true`. `/memory` uses this for the
memory observability dashboard. Batch reads reuse one DuckDB setup for a
small group of widgets (max 4 SQL statements), wait only briefly for the
shared guard, interrupt slow per-SQL items, cap materialized results,
and return per-widget errors instead of failing the whole batch.

To keep ad-hoc dashboard reads responsive, memory event queries lazily
compact completed daily partitions into
`dt=YYYY-MM-DD/_compact/memory_events.compacted.parquet` and then prefer
that single compacted file. Raw `batch_*.parquet` files are left in place
for auditability; current-day partitions stay raw because the sink may
still append there.

After compacted partition files are protected, live queries cap only the
remaining raw Parquet fan-in to the newest
`MAGICIAN_MEMORY_EVENTS_QUERY_MAX_PARQUET_FILES` files (default `6000`).
Operators can raise it for offline/debug runs after raising the process
file-descriptor limit, or lower it if DuckDB reports `Too many open
files`. The append-only lake on disk is not truncated by this cap.

`GET /api/magician/v2/memory/temperature/status` reads the scoped
temperature overlay and hot-projection index and returns lane/tier
inventory, utility-label counts, top entries, and projection lifecycle
reasons.

### Memory retrieval eval runner

`analytics/memory_eval_runner.rs` runs retrieval evals in-process. It
calls the same `render_memory_tiers_for_prompt` path used by agent
loops, then writes one `eval_case` row per case. Eval renders disable
normal retrieval audit emission, so periodic checks do not inflate live
prompt-injection metrics.

The periodic runner is **off unless** `MAGICIAN_MEMORY_EVAL_ENABLED=true`.
Manual scoped run always works:

```text
POST /api/magician/v2/analytics/memory_events/evals/run
```

The `/memory` dashboard exposes the same trigger through **Run evals
now**. Scheduling defaults when enabled: first run after 60 seconds,
repeat every 6 hours; override with
`MAGICIAN_MEMORY_EVAL_STARTUP_DELAY_SECS` and
`MAGICIAN_MEMORY_EVAL_INTERVAL_SECS`.

Suite load order:

- system suites from `magician_data_v3/system/memory_evals/*.json`
- scoped suites from
  `magician_data_v3/scopes/<principal>/<workspace>/memory/evals/*.json`
- compiled-in suites from `data/magician_v2/memory_evals/*.json`
  (core smoke, personal-assistant, internal-system-analyst,
  simple-data-analyst, web-researcher, memory-lifecycle, meetings
  (ships `enabled: false`), weg-ambient with `skip_if_absent`)

A scoped file with the same `suite_id` can override a built-in suite.
Each case must provide either `expected_substrings` or
`min_selected_count`. Default scopes are `user`, `agent`, and
`agent_goal`. Optional `goal_id` only affects `agent_goal` rendering.

## Activity Spine

The activity spine is the durable counterpart to the live runtime
activity stream: one row per **completed** span, from every first-party
span the tracing layer already sees.

```text
<scope>/analytics/activity_rows/dt=YYYY-MM-DD/hour=HH/batch_<ulid>.parquet
<scope>/analytics/activity_rollups/dt=YYYY-MM-DD/rollup_<source-hash>.parquet
```

A rollup object's name is derived from the sources it summarises rather
than being random, which is what makes an interrupted retention pass
rewrite its own object instead of publishing a second one for the same
rows. See
[storage-governance.md](storage-governance.md#the-activity-spines-two-level-fold).

### Losing a row is counted, and reported

The submit path is bounded and non-blocking — observability must never
stall the thing it observes — so a stalled disk costs rows. Rows are
lost in three distinct ways, counted separately: the submit queue was
full, the blocking writer task died, or one `(dt, hour)` Parquet write
failed. The sink warns on its flush cadence, and again at shutdown,
whenever any of those counters has moved.

Shutdown awaits the activity forwarder before draining the sink.
Cancelling the token asks the producer to stop; it does not observe that
it has, and draining first would leave closes in a queue nobody reads
again.

Rows are written on close and never on start, so a row is never
half-populated. A span is filed under the hour it **started** in — a
span that ran 03:00→07:00 was working at 03:00, and filing it at finish
would leave that hour looking idle.

Columns: `activity_id`, `parent_activity_id`, `root_activity_id`,
`name`, `target`, `kind`, `workload_class`, `priority`, `principal`,
`workspace`, `agent_id`, `thread_id`, `task_id`, `model`,
`started_at_ms`, `duration_ms`, `outcome`, `dt`, `hour`.

Deliberate absences: **no cost** (join `llm_dispatch` on `activity_id`;
the spine *does* carry `model`); a malformed `activity_id` on
`LlmCallCompleted` is normalised to absent; the durable record is
submitted before the live `ActivityCost` event. **`workload_class` NULL
means undeclared** and stays NULL. **`priority` is currently always
NULL** — the column exists so filling it later is not a schema
migration.

`root_activity_id` is denormalised so "everything this agent turn cost"
is a flat `GROUP BY` instead of a recursive CTE.

#### Which spans declare `workload_class`

`workload_class` is **inherited from the nearest declaring ancestor**,
never guessed. `operation_llm_router`'s `llm_dispatch` span declares
none on purpose: it is a mechanism serving whoever called it.

| Declaring span | Class | Why |
| --- | --- | --- |
| `chat_turn` (`chat::service::process_chat_inline_turn`) | `foreground_chat` | Every inbound turn converges here. A user is blocked on the reply. |
| `agentic_run` (`execution::agentic::executor`) | `interactive_task` **or nothing** | `interactive_task` when `chat_inline` is set or `chat_session_id` is present. Otherwise undeclared. |
| `observable_source_run` (`content_sources::observation_runtime`) | `ambient` | Deterministic polling of subscribed sources. Same class on `run_now`, the scheduled sweep and startup catch-up. |
| `ambient_distill_pass` (`api::ambient_api`) | `ambient` | Distilling passively captured browsing. |
| `mail_classify_pass`, `mail_distill_scope_tick` (`magician-comms` `channel_assist::assist`) | `comms_assist` | Timer loops that reach the operation router directly. |
| `memory_consolidation_cycle` | `memory` | |
| `taste_capture_sweep`, `attention_rank_recompute_pass` | `scheduled` | |

Every value above is one of the nine `WORKLOAD_*` constants in
`analytics/runtime_activity_layer.rs`, whose values are exactly what
`magicllm::LlmWorkloadClass::as_str` returns. They are a **join key**
against `llm_dispatch` `workload_class`, not a display label.

`agentic_run` stays silent for a run with no chat lineage.
`AgenticContext` carries no trigger, so guessing `autonomous_task` would
preempt a `scheduled` a monitor above could declare. The single
exception is an invocation a server route explicitly marked
`InvocationSourceKind::Autonomous`. Each row of the table is held by a
test in `runtime_activity_layer.rs`.

Query endpoints:

```text
POST /api/magician/v2/analytics/activity_rows/query
POST /api/magician/v2/analytics/activity_rows/query_batch
```

They install **two** scoped tables in one setup — `activity_rows` (full
rows, 7 days) and `activity_rollups` (per-hour summaries, 13 months) —
kept separate rather than UNION-ed. A missing tier materializes as an
empty table of the right shape. `ActivityRowsQueryResponse` adds
`inventory_complete`. Both tiers read with `hive_partitioning = false`
(`dt` and `hour` are real columns). File selection goes through
compaction manifests, not a glob. Projected identifiers are quoted
because `activity_rollups.count` collides with a DuckDB function.

`/runtime` hydrates history from `activity_rows/query`
(`ui/unified-ui/src/routes/(app)/runtime/ActivityView.svelte`). `/llm`
economics still read the `llm_calls` compatibility relation.

Partitioning, the two folds and the tiered retention are in
[storage-governance.md](storage-governance.md#the-activity-spines-two-level-fold).

## Agent DuckDB Tool Safety

Agent-facing DuckDB surfaces reuse the analytics safety boundary. The
generic DuckDB executor and the `internal_data` analytics actions
acquire the process-wide analytics DuckDB guard before blocking work,
apply the checked connection setup, interrupt long-running statements at
the action deadline, and keep the capacity permit on the blocking thread
until DuckDB actually exits. Result materialization is bounded at
10,000 rows / 4 MiB before JSON/table/CSV formatting, with truncation
notices for narrowed follow-up queries. This keeps wide `llm_calls`,
`memory_events`, logs, learning-feed, and ad-hoc DuckDB scans usable
for agents without letting an abandoned or oversized query starve the
UI analytics lane.

## Coding-run lifecycle metrics

VibeDev coding runs emit durable, scope-attributed analytics events
alongside the ephemeral V3-bus `coding.*` events (the bus drives the
cockpit; these are the queryable record). Emitted from
`execution/compiled_handlers/run_coding_task.rs` via
`analytics::emit(...)` (no-op when the analytics layer is uninitialized):

| `event_type` | When | Payload (besides scope) |
|---|---|---|
| `coding.started` | a coding turn begins | `engine`, `plan_run`, `pi_session_persisted`, `continuation` |
| `coding.completed` | the turn finishes | `engine`, `no_change`, `plan_run`, `pending_approval`, `event_count` |
| `coding.stats` | session stats present | `engine`, `cost`, `tokens`, `context_usage`, `total_messages`, `tool_calls` |
| `coding.failed` | the turn errors | `engine`, `stage`, `error`, `termination_cause`, `budget_stop` |

Query: `SELECT * FROM events WHERE event_type LIKE 'coding.%'`.
