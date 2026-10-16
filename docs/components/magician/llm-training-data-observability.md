# LLM Training-Data Observability

- `magicllm` owns provider-call facts: request receipt, provider selection and
  attempt identity, token/cache usage, finish state, retry/fallback, TTFT and
  timing. It must not depend on Magician concepts (tasks, chat, memories,
  feedback).
- Magician owns product semantics: principal/workspace scope,
  task/execution/step and chat-turn lineage, tool effects, validation, delivery,
  feedback, outcomes, privacy, analytics storage, training eligibility and
  dataset snapshots.

`magicllm` mints root/call/attempt identity before routing; Magician supplies
scope and lineage before dispatch and carries the receipt into canonical events,
so `magicllm` never depends on the runtime. Canonical capture is process-owned
and on by default. Design: [LLM Trace, Outcome, Acceptance, And Training
Data](../../plans/2026-07-11-llm-training-data-and-observability-design-plan.md).

## Contracts

Under `data/magician_v2/llm_observability/`:

- `coverage-ledger-v1.json`: every production invocation boundary or explicit
  exclusion.
- `operation-families-v1.json`: every configured operation in exactly one
  outcome/validator family. The `intentional_mapping_variance` (`repo_only` /
  `template_only`) block is empty and unread; the audit diffs only the repo-root
  `magician-config.yaml` seed, and fails on any production operation key
  (`LLMOperation::Other("…")` literal or `*OPERATION*` constant, tests excluded)
  missing from the contract, since it would silently ride the router default
  profile. Notable: `relentless_actor` → `drafting_and_state_synthesis`,
  `relentless_critic` → `safety_and_review` (validated in
  `relentless_runtime.rs`; classification grants no delivery authority);
  `embedding` covers `embed_documents`/`embed_query`
  (`docs/archive/plans/2026-08-24-llm-chokepoint-closure.md`);
  `memory_connection_review` and `memory_lifecycle_review` →
  `safety_and_review`.
- `sink-contract-v1.json`: buffering, scope, durability and loss for
  `llm_calls` and `llm_dispatch`.
- `phase2-{fact,durability,materialization,governed-read,activation,product-access}-contract-v1.json`,
  `phase3-sanitized-content-contract-v1.json`,
  `phase4-tool-lineage-contract-v1.json`.

| Command | Checks |
|---|---|
| `make llm-trace-coverage` | Provider-free drift gate: source anchors, operation assignment, validator sources, sink contract. Also run by `make test-llm-trace-phase0`. |
| `make llm-trace-phase0-baseline` | Read-only Parquet scan (schema, counts, timestamps, operations; no content); e.g. `LLM_TRACE_PHASE0_ARGS='--runtime-root ~/MagicianNotes --principal anonymous --workspace default --strict'`. Its `success` is a transport signal, not an outcome. |
| `make test-llm-trace-phase{0,1,2,2b,2c,2d,2e,2f,3,4}` | Provider-free invariant matrices (`make test-llm-trace-phase1`, `make test-llm-trace-phase2b` for the recovery index, `make test-llm-trace-phase4`, …). |
| `make llm-trace-phase1-audit` | Live historical join; reports `no_phase1_data` rather than inventing coverage. |
| `make llm-trace-phase2f-audit` | Canonical vs compatibility rows by `llm_call_id`: exact tokens/latency, bounded cost, pricing provenance, per-call attempt accounting, ≥ 99% queued-timing joins, consistent overview, content-free categories. |
| `make llm-trace-phase3-audit` | Never stores or prints payloads; `LLM_TRACE_PHASE3_ARGS` picks a non-guest cohort or call; the API probe needs `--call-id` and `MAGICIAN_SETUP_TOKEN`; metadata-only reports skipped. |
| `make llm-trace-phase4-audit` | Aggregate counts and machine categories only; no stable ids or payloads. |

Reports: `$(COVERAGE_BASE_DIR)/evals/llm-observability-phase*/latest/` (repo
`coverage/` symlink). Historical call rows lack stable
call/attempt/dispatch/turn identity and capture/validation status.

## Identity and correlation

Every direct or queued request carries `LlmTraceContext`: root `trace_id`,
logical `llm_call_id`, attempts `{llm_call_id}:a{n}`, `dispatch_job_id`,
authoritative scope, and any
task/execution/plan/step/iteration/chat-turn/parent/retry/workload lineage.

- Provider retries keep the call id; caller retries get a new one in a shared
  retry group; idempotent subscribers keep the owner's identity, marked reused.
- The attempt counter advances before every provider invocation (fallback:
  `a1`, `a2`). Results and terminal errors carry the effective
  profile/provider/model; a later fallback/preflight failure keeps the last
  physical route. Queue health and cooldown follow the effective provider.
- Chat tool-loop calls share the user turn as root trace; agentic decisions
  share the root execution with a stable iteration id.
- `chat_session_id` comes from the task manifest (never an execution-id pattern;
  plan mode writes it there) and is carried through events, pause/resume and
  runtime-owned tool dispatch; model-supplied provenance is never trusted.
  Autonomous tasks leave it empty.

**Turn lineage is stamped at the source.** Call, attempts and tool executions
are stamped from one `LlmTraceContext`; inline chat turns get
`execution_id = chat_turn_id` from `chat_turn_trace_context`. Lineage set only
on `LLMResponseReceived` fails the read service's cross-dataset guard (tool rows
must match their call on every shared column).

| Boundary | Rule |
|---|---|
| Streaming chat | Context and attempt counter minted before awaiting, so a dropped future closes that call. Failures use the typed error's effective route; configured profile is a hint only for zero-attempt/preflight failures, never after fallback. |
| Realtime voice | Identity and start minted at the response boundary, reset per utterance; meetings use per-response identity. Missing usage stays unknown; timeout/empty audio and provider errors are failed calls (content-free class); session/config errors outside a response are not. |
| Usage splits | OpenAI adapters (proxied and browser-direct) accept a detailed split only if all buckets are non-negative integers, cache fits modality and sums match; else coarse totals, unknown pricing. |
| Browser-direct WebRTC | Completion closes the call even without usage; failure, incomplete, cancel, interrupt, barge-in and session close emit a bounded failure terminal; raw provider error text stays in local diagnostics. Hands-free voice is excluded (billed as its chat call). |
| Chunked requests | `chunk_map` / `chunk_repair` / `chunk_fallback` / `chunk_reduce` are typed children. Scope and lineage never reach provider bodies. |

`llm_calls` and `llm_dispatch` Parquet persist these columns (dispatch
partitioned by scope). The DuckDB `llm_calls` view uses `union_by_name`, filling
only absent columns; SQL `NULL` stays unknown (never zero tokens/cost or a
invented route). Compatibility rows never persist reasoning or free-form errors
(fixed failure marker and machine category).

## Canonical recorder, journal, and materializer

`LlmTraceRecorder` takes call-start, provider-attempt, call-completion and
capture-gap records: `call_fact` revisions 1–2, `provider_attempt` revisions 1–3
(start, first token, completion), keyed by `(record_kind, stable_id, revision)`.
Records hold timing, normalized token classes, versioned pricing source,
finish/refusal/truncation, validation and capture status, never content, and are
validated before any sink; operation, capability and routing categories are
bounded machine tokens. Envelopes carry a monotonic sequence and BLAKE3
checksum, ≤ 16 KiB each. Token invariants: cache-read + cache-creation ≤ input;
uncached audio + all cache ≤ folded input; realtime splits cover every bucket
and claim no cache-creation.

- **Lanes and worker.** Bounded lanes for critical facts (all current facts),
  lineage/outcomes and restricted payloads (larger than critical is a config
  error). The hot path never blocks. A worker drains critical first, appends
  JSONL to fsynced per-scope segments, then materializes batches and advances a
  checksum-bound watermark only on success; failed publication stays on disk.
- **Validation is a separate fact.** Plain text: unattempted. Structured callers
  emit typed success/failure; stored errors are a class plus a fixed message.
- **Recovery index.** `recovery-index.sqlite3` holds identities, checksums,
  offsets, lifecycle state and parent edges, never payloads; it commits after
  journal fsync, so a lost transaction leaves a replayable suffix. Later starts
  decode only the unindexed suffix. Changed sealed segments, missing files, bad
  checkpoints and SQLite corruption fail closed. Repair: stop the owner, rename
  the index and sidecars, rebuild. **Never delete the canonical journal or
  report Parquet files to repair an index.** A journal moved to another
  filesystem needs a rebuilt index.
- **Bounds.** Batches ≤ 256 records / 8 MiB; 1 MiB page cache, no mmap; ≤ 8
  scope indexes open; segments ≤ 64 MiB. A partial final line is repaired; gaps,
  checksum mismatches and conflicting deliveries fail closed; same-checksum
  redelivery dedupes.
- **Loss evidence and shutdown.** Saturation counts exact
  principal/workspace/operation/reason groups and emits a typed `capture_gap`
  at the next free slot or shutdown. Shutdown (or dropping the owner) closes
  admission, drains by priority and reports leftovers past its deadline.
- **Writer lease.** One process holds it; startup waits for index validation
  and never abandons a running writer on timeout.
  Reads warn of stale analytics when indexed revisions lead the watermark.
- **Materialization.** `analytics/llm_calls/dt=YYYY-MM-DD`,
  `analytics/llm_provider_attempts/dt=YYYY-MM-DD`,
  `analytics/llm_capture_gaps/dt=YYYY-MM-DD` (UTC). Names derive from the
  revision key, so replay never duplicates. Rows are written through a
  content-free schema with fsync, rename and read-back before the watermark
  moves; a crash between publish and watermark is repaired on replay; same key
  with a different checksum fails closed. Raw revisions stay one-row objects
  that compaction never deletes or mutates.

## Governed reads

Completed UTC partitions (never the current one) over 64 raw files (configurable
default) are compacted under a manifest of raw names, lengths, BLAKE3 checksums
and journal range. A read uses compacted or raw, never both; a bad manifest or
changed raw set falls back to raw.

`LlmAnalyticsReadService` owns the exact scope path boundary, stable schema,
latest-revision coalescing, filters, pagination, totals and overview. Allowlist
(exact relation and column names): `llm_calls`, `llm_provider_attempts`,
`llm_capture_gaps`, `llm_call_revisions`, `llm_provider_attempt_revisions`;
empty datasets keep the typed schema. Historical `batch_*.parquet` rows carry a
`legacy_schema` training exclusion and cannot override scope. Typed reads
default to a 24-hour half-open range, reject ranges over 31 days and open only
intersecting partitions; negative or non-finite aggregates fail closed.

Errors: contended DuckDB guard (`LLM_READ_GUARD_TIMEOUT_MESSAGE`) → `503` with
`Retry-After`; other failures `500`; `400` only for client faults (missing/unsafe
scope or unsafe legacy SQL).

Routes under `/api/magician/v2`:

- `GET /analytics/llm/overview`, `/catalog`, `/schema`
- `GET /analytics/llm/calls`, `/calls/{llm_call_id}`, `/provider-attempts`,
  `/provider-attempts/{provider_attempt_id}`
- `POST /analytics/llm/facts/query`
- `GET /analytics/llm/traces`
- `GET /analytics/llm/traces/{trace_id}`
- `POST /analytics/llm/content/grants`
- `GET` + `DELETE /analytics/llm/content/calls/{id}` (grant via
  `X-Magician-Content-Grant`)

The `/llm` capture-health band shows calls vs attempts, stage timing, validation
coverage, freshness, gaps and telemetry coverage. Cost sums the known subset in
USD with observed pricing versions and sources; `usage_observed_calls` and
`cost_observed_calls` keep missing pricing distinct from zero (as does
crew-health spend).

`internal_data` actions: `llm_observability_overview`, `list_llm_calls`,
`read_llm_call`, `list_llm_provider_attempts`, `read_llm_provider_attempt`,
`query_llm_facts`, `list_llm_traces`, `read_llm_trace`; `read_llm_call_content`
is deliberately absent. All share one
scope/range/filter/freshness/coverage/pagination/warnings/data envelope.
Runtime-injected `__principal` and `__workspace` are mandatory and
authoritative; public values may only repeat them.

**Fact query** parses one `SELECT`/`WITH` into an AST over the registry and
local CTEs only, rejecting shadowing, self references, qualified relations,
table functions, `SELECT INTO` and non-SELECT bodies at every node, with DuckDB
external access off. Bounds: 31 days, 1,000 rows, four MiB per response;
aggregates report a limit probe as explicit censoring.

Legacy `query_llm_calls` (`POST /analytics/llm_calls/query`, `/query_batch`, and
`internal_data`) has its own AST allowlist and content-free projection: only
`llm_calls`, `chat_session_cache_summary` and valid CTEs; caller CTEs cannot use
the `__llm_fact_bounded_*` namespace.

## Activation and reconciliation

Canonical capture and the compatibility mirror subscribe at boot. Each scoped
`LLMResponseReceived` becomes call-start, final-attempt and completion
revisions; agentic calls keep `iteration_id` and record
`prompt_projection_mode` (`bootstrap`, `continuation`, `rebootstrap`).

The bridge never infers missing telemetry:

| Condition | Result |
|---|---|
| Missing scope, correlation, attempt count/id or absolute start | `runtime_event_mapping_rejected` gap; start is never completion minus latency |
| Earlier attempts reported but their events gone | Final attempt only; `earlier_provider_attempt_lifecycle_unavailable` |
| Invalid TTFT or cost | Usage repriced from token buckets and the effective-dated table; `producer_cost_mismatch_recomputed`, or `invalid_provider_usage_omitted` for impossible buckets |
| `response_reused` subscriber | Counted, no second lifecycle |
| Success without effective provider/model | Pricing unknown; `terminal_provider_attempt_lifecycle_unavailable`; configured profile never becomes the route |
| Failure with exact effective route | Final failed attempt, unknown usage; earlier attempts gapped |

`analytics reprice-llm-calls` applies the same arithmetic to history, skipping
unknown routes, invalid values and coarse realtime rows. Free-form diagnostics
map to `mapping_validation_failed`.

Broadcast lag is `unclassified_transport_events_lost` under
`anonymous/default`: a strict audit failure outside the coverage denominator.
`known_missing_fact_revisions` counts only critical drops and unavailable
earlier attempts; other diagnostics appear in `capture_gaps`. Call-specific
gaps carry their `llm_call_id`; lag and saturation gaps are unowned.

Startup recovers existing scope indexes before new commits. Duplicate delivery
is harmless only with an agreeing checksum. Shutdown joins
producers, drains events and the durable lanes, then flushes the mirror.

The legacy `llm_calls` sink is a best-effort mirror, not a completeness
authority and ignores request-start events; the view hides mirror rows whose
`llm_call_id` is canonical. Scoped `llm_dispatch` stays the source of queue and
execution timing, joined by `dispatch_job_id` and `llm_call_id`. A 90-day UTC
retention sweep covers calls, attempts, gaps and legacy partitions, never
journal segments.

**A retention horizon of zero is rejected, and the sweeper skips it anyway.**
The restricted-content sweeper runs from boot even with tracing off and
re-reads `analytics.llm_trace.retention` hourly; a zero cutoff is today, so it
would wipe
`llm_content_tombstones` and `llm_content_access_audit`, which have no other
copy. Validation rejects zero `facts_days`/`context_metadata_days` regardless of
`enabled`, the sweeper treats zero as unset, and every removal is logged. The
storage-maintenance runtime also runs completed-day and five-minute rolling
compaction; reads use the verified compacted prefix plus a bounded raw tail.
Shutdown joins maintenance after producers quiesce and before the pipelines'
final batches.

## Sanitized and restricted content

`magicllm` emits borrowed observations of the request, each provider request and
each normalized response. Magician applies policy, queues eligible ones in a
64-MiB-bounded lane and sanitizes them on a worker before any write.

- **Policy.** Default `content_mode: metadata`; scope overrides beat operation
  overrides, then global; sampling is stable per call. Anonymous guests stay
  metadata-only. Training eligibility without sanitized capture, zero retention,
  fail-open redaction or raw-local capture is a config error.
- **Sanitizer.** Redacts the scope's secret-store values, private keys, bearer
  forms and token shapes; structured values, dynamic object keys and tool
  identifiers are untrusted content. Media and URLs become references plus a
  per-scope keyed fingerprint;
  reasoning keeps only length and fingerprint; raw bodies and response ids are
  never captured; oversized payloads become `content_omitted`; any failure
  becomes one fixed content-free gap category.
- **Storage.** `analytics/llm_call_io`, `analytics/llm_context_blocks`,
  `analytics/llm_content_tombstones`, `analytics/llm_content_access_audit`, with
  their own journal, lease and retention; not in fact SQL or `internal_data`.
  Payload segments are pruned only after verified publication.
- **Reading.** A setup-token-authenticated one-use grant from `POST
  /api/magician/v2/analytics/llm/content/grants`, sent only in
  `X-Magician-Content-Grant` to `GET
  /api/magician/v2/analytics/llm/content/calls/{llm_call_id}`. The grant binds
  scope, actor, target, reason and expiry server-side; a reveal is re-sanitized,
  capped at 256 revisions / 1 MiB, audited (fsynced marker) before returning,
  then a Parquet audit copy is queued; `no-store`; failures carry fixed machine
  codes. `read_llm_call_content` stays absent until a typed trusted grant can
  reach dispatch without model arguments becoming authority.
- **Deleting.** Setup-token-authenticated
  `DELETE /api/magician/v2/analytics/llm/content/calls/{llm_call_id}` writes an
  fsynced revocation marker (immediate, restart-safe denial) before the
  append-only tombstone, which never expires before the I/O it protects.

## Tool, action and branch lineage

`llm_tool_calls` is an append-only, content-free lifecycle keyed by provider
tool-call id and `llm_call_id` (edge `llm_call_id:tool:model_tool_call_id`).
Arguments are stored only as a scope-keyed HMAC; arguments, results, delegated
content and prompts never enter it.

- Stages run from proposal through parsing, validation, authorization
  ("required" vs "obtained"), approval, execution, result validation, consumers,
  branch materialization and rollback. Execution success and result validity are
  independent. Failure owner, side-effect state, successful-path membership,
  same-argument repeats and observation/action cycles are bounded machine facts.
- Admission failures invent no later stages. Once a transport ran, failure,
  cancellation or timeout leaves side effects `unknown`; only an authoritative
  rollback sets `reversed` or `rollback_failed`; file-edit rollback comes only
  from runtime-authored snapshot-restoration markers.
- Chat and agentic execution share one emitter covering compiled/MCP tools,
  browser and screen actions, delegation (ids only), subgoals and the external
  coding engine.
- Linkage failures emit a content-free gap (lineage-buffer gaps keep their call
  id). Recovery is attributed only after
  an authoritative transport failure; each later call consuming a retained
  result gets its own edge. Branch success means the local branch materialized.

The governed `llm_tool_calls` relation keeps the ordered lifecycle.
`GET /analytics/llm/traces`, `GET /analytics/llm/traces/{trace_id}`,
`list_llm_traces`, `read_llm_trace` and call detail share one scope-enforcing
assembler; `/llm` has a call explorer with the tool timeline.
