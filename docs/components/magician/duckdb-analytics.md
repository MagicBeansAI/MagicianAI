# DuckDB Analytics Architecture

## Purpose

DuckDB has two isolated roles sharing no state or lifecycle: an internal analytics database for system events, and an agent-facing query capability whose guide is the `guide:` field of `magician/src/magician_v2/execution/embedded_pack_defs/duckdb.yaml` (compiled in with `include_str!`, `execution/compiled_providers.rs`; injected as `pack.guide.as_deref()`), so editing it needs a rebuild.

## Internal Analytics Database

Schemaless events go into one `events` table in `magician_data_v3/scopes/<principal>/<workspace>/analytics/analytics.duckdb` through a system-owned pool. Analytics is optional, and agents read it only through `internal_data` or the HTTP API.

- **Pools.** Lazy. `AnalyticsDispatcher` owns one read-write `DuckDbPool` per `(principal, workspace)`; reads use a coexisting `read_connection()` (a second read-write handle races the file lock). A failed open is logged and skips that write.
- **No extension loading.** Binaries statically link pinned JSON and Parquet (`duckdb = "=1.10502.0"` with `bundled`, `json`, `parquet` in `magician/Cargo.toml`): no autoload, user cache or network install. After server views install, queries set `enable_external_access`, `autoinstall_known_extensions`, `autoload_known_extensions` to false.
- **Writes.** `analytics::emit()` (`analytics/mod.rs`) is fire-and-forget, a no-op without a dispatcher. The sink (`event_sink.rs`) holds 10_000 events, flushes every 100 events or 500 ms, drops with a counter when full.
- **Parquet mirror** at `<scope>/analytics/events/dt=YYYY-MM-DD/batch_<ulid>.parquet`. An unreadable `analytics.duckdb` is renamed `analytics.duckdb.incompatible-<ts>` (never deleted) and rebuilt from the mirror; lock conflicts are not corruption.
- **Retention.** Rows newer than 24 hours, and always the newest 1000 per `event_type`, whichever keeps more; swept every 5 minutes.
- **Schema catalog**: `<scope>/analytics/schema_catalog.json`.
- **Read-only API**: `/api/magician/v2/analytics/schema`, `/api/magician/v2/analytics/query` (`magician-api/src/analytics_api.rs`, registered in `magician-bin/src/main.rs`); 503 without `AnalyticsApi`.

Every analytics parse site must use `parse_analytics_sql` (`analytics/llm_sql_guard.rs`), never `Parser::parse_sql`: it caps nesting at `ANALYTICS_SQL_RECURSION_LIMIT` (20), because `sqlparser`'s default of 50 overflows an actix worker's 2 MiB stack and aborts the process.

Pulse reads bind memory-event schemas before materializing only the requested timestamp window; numeric bounds also prune partition selection. Recent LLM windows materialize narrow source columns before reconciliation and reuse its snapshot for repeated aggregates, under the existing memory, spill and external-access limits. An `llm_calls` `query_batch` copies its whole reconciled window into memory once (see Concurrency).

## Idle workspace resources

Feed, UI-thread, Channel Assist and analytics owners share an idle cache: a 15-second collector releases owners idle 60 seconds, keeping at most eight idle per cache (active requests may exceed it). Queries, transactions and native read connections pin their owner; closing runs outside the cache mutex and a same-workspace lookup waits for it. Files and rows are kept. Startup releases unused Feed/UI-thread owners; analytics expiry closes the sender and drains accepted events before releasing the pool, which closes once readers and flushes finish (possibly one more idle interval). SQLCipher App registry connections also expire after 60 idle seconds, sparing checked-out leases.

Thread and memory limits are set before any native instance opens, including read-only connections and reopened owners: UI-thread databases one thread and 256 MiB, analytics its checked budget, Feed and Channel Assist their configured budgets. The collector and flushes use blocking admission, not a pool per workspace; no YAML is needed. Tests: `make test-workspace-resource-lifecycle`, `make test-app-registry-admission`.

## Event Model

Payloads evolve without migrations. `AnalyticsEvent` (`event_sink.rs`) constructors: `log`, `bot_log`, `chat_session`, `chat_message`, `task_execution`, `task_step`, `artifact_registered`, `artifact_transition`; coding-run events use the same `analytics::emit()`.

## Convenience Views

`views.rs` creates idempotent `CREATE OR REPLACE` views over `events` (`logs`, `bot_logs`, `chat_messages`, `chat_sessions`, `task_executions`, `task_steps`, `artifacts`) plus best-effort memory views (`episodes`, `memory_tiers`) that scan JSON with `read_json_auto()`, skipped when the directory is absent. `internal_data` attaches it read-only (`query_events`, `tail_logs`, `find_errors`).

## Agent-Facing DuckDB Capability

The `duckdb` pack (`implementation.type: primitive`, `provider_name: duckdb`) never touches the internal connection. By default it uses a session-scoped in-memory connection (`DuckDbSession`, `execution/native_executors.rs`) whose tables last across actions; with `database` set it opens that `.duckdb` file freshly per action, so temp tables do not survive.

### Inner-loop actions

Actions lower through `DuckDbCapabilityProvider::lower` → `lowering::lower_duckdb_action`; `CompiledProviderDispatcher` (`execution/primitive_dispatch/compiled_provider.rs`) injects `__action_name`, `__principal`, `__workspace`, `__task_id`, and no `__action_name` means `query`.

| Action | SQL and rules |
|---|---|
| `query` | Full SQL. Default. |
| `preview` | `SELECT * FROM '<source>' LIMIT <n>`; quotes escaped; non-positive `limit` → 1. |
| `describe` | `DESCRIBE SELECT * FROM '<path>'`, or `DESCRIBE <table>` with `is_table=true`; identifiers `[A-Za-z0-9_]`. |
| `list_tables` | `SHOW TABLES`; default `output_format` `table`. |
| `read_parquet` | `SELECT <select> FROM read_parquet('<path>', hive_partitioning = <bool>)` + optional `WHERE`/`LIMIT`; `select` `*`, `hive_partitioning` `true`. `llm_calls@<principal>/<workspace>` expands to `magician_data_v3/scopes/<principal>/<workspace>/analytics/llm_calls/dt=*/*.parquet`. |
| `export` | `COPY (<sql>) TO '<output_path>' (FORMAT '<format>'[, HEADER true])`; only `csv` / `parquet` / `json`; `HEADER true` only for CSV when `header=true` (default). |
| `attach` | `ATTACH '<attach_path>' AS <alias> [(READ_ONLY)]`; alias `[A-Za-z0-9_]`, both non-empty; `read_only` default `true`; `attach_path` is its own parameter, separate from the session `database`. |

## LLM Calls Parquet Lakehouse

Append-only LLM telemetry sharing no pools or schema with the scoped `analytics.duckdb` stores (bounded by per-event-type retention); each compatibility query opens a fresh in-memory DuckDB over the Parquet cold store, and the runtime does no aggregation. Canonical facts are a third, journal-backed surface sharing pools with neither. Two writers subscribe to `RuntimeTransportEvent::LLMResponseReceived` on `RuntimeTransportBroadcaster` (which also feeds SSE, `events.jsonl` and `transport_log.jsonl`):

- **Canonical facts**: `LlmTraceActivation` (journal + materializer, on by default), read through `LlmAnalyticsReadService` (`/api/magician/v2/analytics/llm/*`, `internal_data` typed actions / `query_llm_facts`).
- **Compatibility mirror**: `LlmParquetSink` (`analytics/llm_parquet_sink.rs`), so `/llm` and `POST /api/magician/v2/analytics/llm_calls/query` keep working. Schema lives in the files (reconciled with `union_by_name`); retention is a directory delete.

### Write path (compatibility mirror)

The sink buffers per `(principal, workspace)` and flushes every 30s, at 100 rows in any scope, or at shutdown, via an in-memory DuckDB appender and `COPY ... TO '<scope>/analytics/llm_calls/dt=YYYY-MM-DD/batch_<ulid>.parquet' (FORMAT PARQUET, COMPRESSION 'zstd')`, dated by the first row's `timestamp_ms`. A `Lagged` subscriber logs "broadcaster lagged; dropped N events" and continues. Writes use checked setup (`threads = 1`) and wait up to 10s for the shared analytics guard, then write without it. Chat-inline calls (including Personal Tutor and Live Concept Tutor) emit like autonomous runs; overlay-only tools such as `screen-draw` emit nothing.

### Pricing

`magicllm::compute_cost_at` sets `cost_usd` at emit time from an effective-dated table: code defaults (`PricingTable::defaults`: per-provider catch-alls, free engines, web-search fees) under the seed `magician_data_v3/llm_pricing.template.json` (compiled in) under `<MAGICIAN_ROOT_DIR>/llm_pricing.json` (the seed on disk in dev, via `runtime_config_path`). Every per-model rate is a file row. `magician-bin/src/main.rs` calls `llm_pricing_config::load_and_install_llm_pricing()` right after config load, since the pricing `OnceLock` locks in the built-in table on first use. Fail-open: a bad or missing file logs one warning.

- Rows effective at the call's `started_at_ms` count; longest `model_prefix` wins, then latest `effective_from`. Price changes are new dated rows; stored costs are frozen.
- `prompt_tokens` includes cache-read and cache-creation; uncached input bills at the input rate, each cache bucket once at its own rate (or `input_per_m`). When `prompt_tokens` is strictly greater than a row's `long_context.threshold_tokens`, its input multiplier applies to uncached and cached input and its output multiplier to output; rows may model a cache-write bucket. Schema 2 adds `realtime_rates`, `web_search_rates` and `media_rates` (TTS/STT). Base rows are dated `1970-01-01` and carry exact floats: each call records its row's fingerprint, so rounding a value changes the fingerprint.
- **Repricing**: `magician analytics reprice-llm-calls` (`make analytics-reprice-llm`) recomputes at each row's `timestamp_ms` and atomically rewrites changed files only (staging sibling, `fsync`, row-count check, rename, directory `fsync`). Dry run unless `--apply`; `--principal`/`--workspace`, inclusive `--from`/`--to`; today's (and future) partitions skipped unless `--include-today`; blank provider/model rows are skipped. No file lock: rows appended mid-rewrite are lost.

#### Operator runbook: updating prices

Change prices in the seed, `magician_data_v3/llm_pricing.template.json`: a dev checkout with no runtime copy reads it at startup, so a pull and restart applies them. `<MAGICIAN_ROOT_DIR>/llm_pricing.json` is for machine-local overrides only; it wins a full tie with the seed, so a full copy goes stale and hides later corrections. Add revisions as dated rows; reprice late ones with `magician analytics reprice-llm-calls --from <date> --apply` after a dry run. A new model needs its own matching `model_prefix` row, dated from first use; otherwise it can inherit a broader model/provider rate or fall through to $0. A variant inherits a parent model’s long-context multiplier only when its provider publishes the same threshold. Retired catalog models keep their effective-dated pricing rows for historical usage. A new provider uses the exact magicllm provider string; empty `model_prefix` is its catch-all, and absent cache fields bill cached tokens at the input rate.

### Compatibility schema

`llm_calls` (`legacy_llm_compat.rs`), one row per call. Defaults apply only to physically absent columns (a stored `NULL` stays unknown). `reasoning_summary` is omitted, a stored `error` becomes `legacy_error_redacted`, non-finite `cost_usd` becomes `NULL`, and an off-charset `response_kind` becomes `invalid_category_redacted`.

| Column | Type | Notes |
|---|---|---|
| `timestamp_ms`, `started_at_ms`, `latency_ms`, `ttft_ms` | BIGINT | End, start, duration, optional TTFT |
| `principal`, `workspace` | VARCHAR | Scope |
| `schema_version` | INTEGER | |
| `trace_id`, `llm_call_id`, `provider_attempt_id`, `dispatch_job_id`, `parent_call_id`, `parent_relation`, `retry_group_id`, `route_decision_id` | VARCHAR | Lineage |
| `scope_resolution` | VARCHAR | `'legacy_default'` when absent |
| `root_execution_id`, `iteration_id`, `prompt_projection_mode`, `chat_turn_id` | VARCHAR | |
| `workload_class` | VARCHAR | `'system'` when absent |
| `call_role` | VARCHAR | `'primary'` |
| `provider_attempt_count` | INTEGER | `0` = bookkeeping aggregate, not a model call |
| `response_reused` | BOOLEAN | |
| `execution_id`, `task_id`, `plan_id`, `step_id`, `agent_id`, `delegated_agent_id`, `chat_session_id` | VARCHAR | Nullable |
| `step_index` | BIGINT | Nullable |
| `operation`, `profile`, `provider`, `model`, `capability` | VARCHAR | |
| `response_kind` | VARCHAR | `tool_call` / `text` / `streaming` / `reasoning_only` / `error` |
| `attempt` | INTEGER | 1-based; > 1 = retry |
| `success` | BOOLEAN | |
| `error` | VARCHAR | Redacted marker or NULL |
| `input_tokens`, `output_tokens`, `reasoning_tokens`, `cache_read_tokens`, `cache_creation_tokens` | BIGINT | |
| `cost_usd` | DOUBLE | |
| `dt` | VARCHAR | Hive partition date when present |

Batches also store audio token splits, not projected by `llm_calls`.

### Emit-site coverage

Both the Parquet sink and the canonical journal subscribe to `LLMResponseReceived`:

- **Autonomous** (`execution/agentic/run_loop/phases/decide.rs`): every decision call; `dispatch_execution_native_messages` (`execution/agentic/native_integration.rs`) stamps provider/model on `ExecutionNativeRouterResponse` → `ExecutionNativeResponse`; `cost_usd` via `magicllm::compute_cost_at` with `started_at_ms` stamped at telemetry construction.
- **Chat-inline** (`chat/service.rs::process_chat_inline_turn`): every call, success or failure; successful rows resolve the profile to provider/model, keep provider token/cache/reasoning usage and price via `compute_cost_at`; filter `capability = 'chat.inline'`.
- **Scoped direct operations** (`analytics/operation_llm_telemetry.rs`, `query_analysis/operation_llm_router.rs`): router calls outside chat and the agentic loop; requires explicit principal/workspace and keeps provider, model, profile, token/cache/reasoning usage, start time and priced cost; no row without router telemetry. Parent-owned chat and agentic calls bypass it, so nothing is double-counted. Channel/mail helpers emit via `llm_dispatch_seam.rs`, voice via `magician-media` `voice_orchestrator.rs`.

Provider-native TTS/STT/VAD/diarization stays on media telemetry unless billable.

### Query path

All under `/api/magician/v2` (not v3), handlers in `magician-api/src/analytics_api.rs`, registered in `magician-bin/src/main.rs` on `web::scope("/api/magician/v2")`:

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

`POST .../llm_calls/query` runs `{ "sql": "..." }` in a fresh in-memory DuckDB over a projection of server-selected files in the caller's scope; the raw Parquet scan is never visible. Response: `{ "columns": [...], "rows": [[...]], "row_count": N }`.

#### One row per call

Governed `call_fact` records (record_revision 1 `started`, 2 `completed`) also land in `llm_calls`, with NULL provider/model (kept on `llm_provider_attempts`). `legacy_llm_compat::build_llm_calls_relation` projects **exactly one row per call** (dropping `started` rows and completed duplicates of legacy-recorded calls): the legacy `batch_*` row if present, else the completed `call_fact` (record_revision = 2) with provider/model from its winning attempt (succeeded first, else last) in `analytics/llm_provider_attempts`. A pure legacy lake (no `call_fact`, no `record_kind` column) reads unchanged.

**Guards.** One `SELECT`/`WITH` statement, AST-walked; only `llm_calls`, `chat_session_cache_summary`, `llm_embeddings`, `llm_cost_daily` and in-scope CTEs (`legacy_llm_compat::ALLOWED_RELATIONS`). Qualified or catalog relations, shadowing, table functions, external scans, `SELECT INTO`, `VALUES`, `TABLE` and mutation-shaped bodies are rejected recursively; external access is off once server views install. Results cap at 10_000 rows / 4 MiB (`ANALYTICS_DUCKDB_MAX_RESULT_ROWS` / `ANALYTICS_DUCKDB_MAX_RESULT_BYTES`) per response or batch; decode errors fail closed.

**Concurrency.** One shared guard and conservative setup (512 MB memory, 1 GB temp), so `/llm` and `/memory` cards queue. UI reads (`query`, `query_batch`, crew health, the typed read service) get `ANALYTICS_DUCKDB_READ_THREADS` (4) because each `llm_calls` query rebuilds the reconciled relation; reads stay serialized by the guard, so this only lets the running query use more cores; writers, compactors and agent queries keep one thread. `llm_calls` `query_batch`: max 16 statements (each further one gets its own `exceeds 16 queries` error result), 5s guard wait. Under the guard it picks files and, when the batch's window is provably within 35 days, runs `CREATE TEMP TABLE _llm_calls_window AS SELECT * FROM llm_calls` once (30s setup budget) and points `llm_calls` at it, then releases the guard and runs each card against memory (8s per item, 60s per batch). The pushed lower bound is the loosest of every card's own (`legacy_llm_compat::llm_calls_timestamp_lower_bound_ms`, AST-checked: a top-level `AND` conjunct `timestamp_ms >|>= <literal or epoch_ms(now()/current_date [- INTERVAL n s|min|h|d|week])>`, relative forms less 2 days of time-zone slack); a card it cannot prove, a wider or lifetime window, or a copy over the memory budget streams as before, holding the guard. The LLM Parquet sink waits 10s for the guard and then writes without it (new files publish by rename); the canonical fact compactor waits 30s.

**`llm_cost_daily`** (`magician-api/src/analytics_api/llm_cost_daily.rs`): spend per `dt`, `task_id`, `chat_session_id`, `provider`, `model` (`calls`, `cost_usd`, five token sums, `first_call_ms`, `last_call_ms`), installed by `query` and `query_batch` when the SQL names it, skipping `llm_calls` when nothing else is named. Each day is reconciled over its own and neighbouring partitions (a call's two copies can straddle them) and cached by file fingerprint (path, length, mtime), so a closed day computes once per process and only days whose files moved are recomputed. Content-free, allowlisted. VibeDev reads it (`CostSource = 'daily'`, `vibedev/llmCost.ts`); `/llm` reads `llm_calls`.

The compiled internal analyst uses typed reads and `query_llm_facts` by default and is instructed not to call `duckdb.read_parquet` on historical `analytics/llm_calls`; the same query-shape guard covers generic/memory analytics SQL and compiled internal event/memory query actions.

### Retention and lifecycle

No per-sink cron and no `analytics.llm_calls_retention_days` key (`LlmParquetRetention` in `llm_parquet_sink.rs` is unused). The single owner, so nothing races verified Parquet publication, is `StorageMaintenanceRuntime` (`analytics/parquet_maintenance.rs`, spawned in `magician-bin/src/main.rs` after the sinks): canonical LLM compaction, completed-day batch compaction, and 90-day retention (`DEFAULT_ANALYTICS_RETENTION_DAYS = 90`, sweep every 6 hours) for `llm_calls`, `llm_embeddings`, `llm_dispatch`, `llm_provider_attempts`, `llm_tool_calls`, `llm_capture_gaps`, `events`, `memory_events` ([storage-governance.md](storage-governance.md)). Shutdown order: canonical LLM drain, `LlmParquetSink::shutdown`, embeddings sink, cancel analytics, activity forwarder, spine sink.

## LLM Embeddings Parquet Lakehouse

On-device Ollama embedding calls go to a **separate** flat dataset so zero-cost, high-frequency telemetry never bloats `llm_calls`.

### Write path

`LlmEmbeddingsSink` (`analytics/llm_embeddings_sink.rs`) writes one content-free record per embed **batch** to `<scope>/analytics/llm_embeddings/dt=YYYY-MM-DD/embed_<ulid>.parquet` (`ArtifactV2Workspace::analytics_llm_embeddings_root(principal, workspace)`). `try_record` never blocks or panics (queue 512, drops counted); a bounded flush channel feeds a DuckDB appender that `COPY ... TO`s zstd Parquet every 30s, at 100 records, or at shutdown after draining the backlog. Completed-day compaction and retention belong to `StorageMaintenanceRuntime`, not the sink. The process-global sink (`set_global_sink`) backs `record_embedding_batch(...)`, a no-op without it. `OllamaEmbedder` (`magician-vector-index`) has no scope, so callers record:

- `resurfacing`: `attention/resurfacing/centrality.rs::EmbeddingCentrality::build`; `input_tokens` is the `len/4` estimate.
- `memory_index`: `analytics/memory_index_maintainer.rs`; `batch_size` = `MemoryLanceDbWriteReport.embedded_rows` (else `chunk_count`), recorded only when `batch_size > 0`; `input_tokens = 0`.
- `procedure_index`: `learning/procedure_index.rs::refresh_procedure_index` after `VectorTable::index`.
- `attention_learning`: `attention/learning/mod.rs`.

`elicitation/manager.rs::embed_metadata` is **not** an embedding site (`elicitation` only writes JSON metadata).

### Schema

| column | type | meaning |
| --- | --- | --- |
| `timestamp_ms` | BIGINT | Batch completion |
| `principal` / `workspace` | VARCHAR | Scope |
| `provider` | VARCHAR | `ollama` |
| `model` | VARCHAR | Configured embed model |
| `operation` | VARCHAR | Purpose tag |
| `input_tokens` | BIGINT | Ollama count, else `sum(len/4)` |
| `batch_size` | INTEGER | Inputs embedded |
| `cost_usd` | DOUBLE | Always `0.0` |
| `latency_ms` | BIGINT | Batch wall-clock |
| `success` | BOOLEAN | Batch succeeded |

### Query path

```text
POST /api/magician/v2/analytics/llm_embeddings/query
```

`query_llm_embeddings_handler` mirrors `query_llm_calls_handler` with a flat `llm_embeddings` TEMP TABLE over `read_parquet(..., hive_partitioning = true, union_by_name = true)` (typed defaults, no reconciliation) and the same guards.

## Memory Events Parquet Lakehouse

```text
magician_data_v3/scopes/<principal>/<workspace>/analytics/memory_events/dt=YYYY-MM-DD/*.parquet
```

| Row | Source | Captures |
|---|---|---|
| `event_kind = 'retrieval'` | Prompt memory retrieval | Query excerpt, agent/goal, scope, tier, item key, score, confidence, injected, candidate/dropped counts, budgets, output size |
| `event_kind = 'consolidation_transform'` | Consolidation | Agent, rule, target, source kind, input/emitted/skipped counts, status, JSON payload |
| `event_kind = 'eval_case'` | Retrieval evals | Suite/case ID, query, pass/fail, expected/matched counts, best rank, status, JSON details |

`memory_parquet.rs` flushes per scoped root every 5 seconds or 100 rows (one `COPY` per flush, shared analytics lock, `threads = 1`).

```text
POST /api/magician/v2/analytics/memory_events/query
POST /api/magician/v2/analytics/memory_events/query_batch
```

These build a scoped `memory_events` view (`union_by_name = true`) for `/memory`; batches run up to 4 statements on one setup, wait up to 5s for the guard, interrupt an item after 6s, cap results and return per-widget errors instead of failing the batch. Completed days are lazily compacted to `dt=YYYY-MM-DD/_compact/memory_events.compacted.parquet` (raw `batch_*.parquet` kept for audit; today stays raw). Compacted files are exempt; live queries read at most the `MAGICIAN_MEMORY_EVENTS_QUERY_MAX_PARQUET_FILES` newest remaining raw files (default `6000`; lower it on `Too many open files`, raise it offline after raising the fd limit); the lake is never truncated.

`GET /api/magician/v2/memory/temperature/status` reports lane/tier inventory, utility labels, top entries and projection lifecycle.

### Memory retrieval eval runner

`analytics/memory_eval_runner.rs` runs evals through `render_memory_tiers_for_prompt`, one `eval_case` row per case, without retrieval audit. The periodic runner is **off unless** `MAGICIAN_MEMORY_EVAL_ENABLED=true` (after 60 seconds, then every 6 hours; `MAGICIAN_MEMORY_EVAL_STARTUP_DELAY_SECS`, `MAGICIAN_MEMORY_EVAL_INTERVAL_SECS`); manual runs always work, including **Run evals now** on `/memory`:

```text
POST /api/magician/v2/analytics/memory_events/evals/run
```

Suites load from `magician_data_v3/system/memory_evals/*.json`, `magician_data_v3/scopes/<principal>/<workspace>/memory/evals/*.json`, and compiled-in `data/magician_v2/memory_evals/*.json` (core smoke, personal-assistant, internal-system-analyst, simple-data-analyst, web-researcher, memory-lifecycle, meetings `enabled: false`, weg-ambient `skip_if_absent`). A scoped `suite_id` overrides a built-in. Cases need `expected_substrings` or `min_selected_count`; default scopes `user`, `agent`, `agent_goal` (`goal_id` affects only `agent_goal`).

## Activity Spine

One durable row per **completed** first-party tracing span:

```text
<scope>/analytics/activity_rows/dt=YYYY-MM-DD/hour=HH/batch_<ulid>.parquet
<scope>/analytics/activity_rollups/dt=YYYY-MM-DD/rollup_<source-hash>.parquet
```

Rollups are named from their sources, so an interrupted retention pass rewrites its own object ([storage-governance.md](storage-governance.md#the-activity-spines-two-level-fold)).

### Losing a row is counted, and reported

Submission is bounded and non-blocking, so a stalled disk costs rows; queue-full, writer-died and `(dt, hour)` write failures are counted separately and warned about on the flush cadence and at shutdown. Shutdown awaits the activity forwarder before draining the sink. Rows are written on close (never half-populated) and filed under the hour the span **started**.

Columns: `activity_id`, `parent_activity_id`, `root_activity_id`, `name`, `target`, `kind`, `workload_class`, `priority`, `principal`, `workspace`, `agent_id`, `thread_id`, `task_id`, `model`, `started_at_ms`, `duration_ms`, `outcome`, `dt`, `hour`. **No cost** (join `llm_dispatch` on `activity_id`); a malformed `activity_id` on `LlmCallCompleted` becomes absent, and the durable record precedes the live `ActivityCost` event. **`workload_class` NULL means undeclared**; **`priority` is always NULL** for now. `root_activity_id` makes a turn's cost a flat `GROUP BY`.

#### Which spans declare `workload_class`

**Inherited from the nearest declaring ancestor**, never guessed; `operation_llm_router`'s `llm_dispatch` span declares none.

| Declaring span | Class | Note |
| --- | --- | --- |
| `chat_turn` (`chat::service::process_chat_inline_turn`) | `foreground_chat` | Every inbound turn |
| `agentic_run` (`execution::agentic::executor`) | `interactive_task` **or nothing** | Only with `chat_inline` or `chat_session_id` |
| `observable_source_run` (`content_sources::observation_runtime`) | `ambient` | `run_now`, scheduled sweep, startup catch-up |
| `ambient_distill_pass` (`api::ambient_api`) | `ambient` | Passive browsing distill |
| `mail_classify_pass`, `mail_distill_scope_tick` (`magician-comms` `channel_assist::assist`) | `comms_assist` | Timer loops |
| `memory_consolidation_cycle` | `memory` | |
| `taste_capture_sweep`, `attention_rank_recompute_pass` | `scheduled` | |

Values are the nine `WORKLOAD_*` constants in `analytics/runtime_activity_layer.rs` (= `magicllm::LlmWorkloadClass::as_str`), a **join key** against `llm_dispatch` `workload_class`; a test there pins each row. `agentic_run` without chat lineage stays silent (guessing `autonomous_task` would preempt an enclosing `scheduled`), except routes marked `InvocationSourceKind::Autonomous`.

```text
POST /api/magician/v2/analytics/activity_rows/query
POST /api/magician/v2/analytics/activity_rows/query_batch
```

These install separate tables `activity_rows` (7 days) and `activity_rollups` (hourly, 13 months; a missing tier is empty); `ActivityRowsQueryResponse` adds `inventory_complete`. Reads use `hive_partitioning = false`, pick files from compaction manifests, and quote identifiers (`activity_rollups.count` collides with a DuckDB function). `/runtime` reads here (`ui/unified-ui/src/routes/(app)/runtime/ActivityView.svelte`); `/llm` economics read `llm_calls`.

## Agent DuckDB Tool Safety

The generic executor and `internal_data` analytics actions take the process-wide analytics guard, apply the checked setup, interrupt at the action deadline and hold the capacity permit until DuckDB exits. Results cap at 10,000 rows / 4 MiB with truncation notices, so wide scans (`llm_calls`, `memory_events`, logs, learning feed) cannot starve the UI lane.

## Coding-run lifecycle metrics

`execution/compiled_handlers/run_coding_task.rs` emits these scope-attributed events via `analytics::emit(...)` (no-op when uninitialized) as the queryable record of VibeDev runs; the ephemeral V3-bus `coding.*` events drive the cockpit.

| `event_type` | When | Payload (besides scope) |
|---|---|---|
| `coding.started` | Turn begins | `engine`, `plan_run`, `pi_session_persisted`, `continuation` |
| `coding.completed` | Turn finishes | `engine`, `no_change`, `plan_run`, `pending_approval`, `event_count` |
| `coding.stats` | Session stats present | `engine`, `cost`, `tokens`, `context_usage`, `total_messages`, `tool_calls` |
| `coding.failed` | Turn errors | `engine`, `stage`, `error`, `termination_cause`, `budget_stop` |

Query: `SELECT * FROM events WHERE event_type LIKE 'coding.%'`.
