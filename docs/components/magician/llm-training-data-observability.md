# LLM Training-Data Observability

Magician's LLM observability is split deliberately across two ownership layers.

`magicllm` owns provider-call mechanics and facts: the effective request receipt,
provider selection and attempt identity, token/cache usage, provider-reported
finish state, retry/fallback details, TTFT and provider execution timing. It must
not depend on Magician concepts such as tasks, chat presentation, memories or
user feedback.

Magician owns product semantics: principal/workspace scope, task/execution/step
and chat-turn lineage, agent/tool effects, validation, what reached a user,
explicit feedback, delayed outcomes, privacy policy, durable analytics storage,
training eligibility and dataset snapshots. `magicllm` creates the generic
root/call/attempt identity before routing; Magician supplies authoritative
scope and product lineage before dispatch and carries the returned receipt into
canonical events and analytics. This keeps `magicllm` reusable and prevents a
reverse dependency on the application runtime.

Current crates: `magician` 0.7.89, `magicllm` 0.2.46. Canonical capture is
process-owned and on by default. Design:
LLM Trace, Outcome, Acceptance, And Training Data.

## Contracts

Machine-readable contracts live under `data/magician_v2/llm_observability/`:

- `coverage-ledger-v1.json` classifies every known production invocation
  boundary or explicit exclusion.
- `operation-families-v1.json` assigns every configured operation exactly once
  to an outcome/validator family. Its `intentional_mapping_variance`
  (`repo_only` / `template_only`) block is empty and no longer read: the repo
  root `magician-config.yaml` is the one seed (the separate distributable
  template was removed), so the audit diffs only that file. The audit additionally scans production Rust code for operation
  keys (`LLMOperation::Other("…")` literals and `*OPERATION*` constants, test
  regions excluded) and fails when a key is absent from the contract — a key
  that exists only in code silently rides the router default profile. The
  `embedding` family covers the routed `embed_documents`/`embed_query`
  operations (profile-bound provider identity through magicllm; see
  `docs/archive/plans/2026-08-24-llm-chokepoint-closure.md`).
  `memory_connection_review` and `memory_lifecycle_review` belong to
  `safety_and_review`: their caller-owned parsers and transition checks validate
  response shape, allowed decisions and bound evidence before publishing a
  connection or changing canonical memory.
- `sink-contract-v1.json` records current buffering, scope, durability and loss
  behavior for `llm_calls` and `llm_dispatch`.
- Phase 2: `phase2-{fact,durability,materialization,governed-read,activation,product-access}-contract-v1.json`.
- Phase 3: `phase3-sanitized-content-contract-v1.json`.
- Phase 4: `phase4-tool-lineage-contract-v1.json`.

`make llm-trace-coverage` is the provider-free drift gate. It fails for a
missing source anchor, missing/duplicate operation assignment, unknown
validator source, or incomplete sink contract (also run by
`make test-llm-trace-phase0`).

`make llm-trace-phase0-baseline` is a read-only scan of the active runtime
Parquet data. Override the scope or runtime root through
`LLM_TRACE_PHASE0_ARGS`, for example:

```sh
make llm-trace-phase0-baseline \
  LLM_TRACE_PHASE0_ARGS='--runtime-root ~/MagicianNotes --principal anonymous --workspace default --strict'
```

The baseline reads schema, counts, timestamps and categorical operation values,
never prompts, responses, tool arguments, attachments, contact data or tool
results. `success` there is a transport/coarse call signal, not a task outcome.
The historical call schema has no stable call/attempt/dispatch/turn identity or
capture/validation status.

Focused matrices: `make test-llm-trace-phase{0,1,2,2b,2c,2d,2e,2f,3,4}`.
Live audits: `make llm-trace-phase{1,2f,3,4}-audit` (Phase 0 uses the
`llm-trace-phase0-baseline` scan above). Reports land under
`/Volumes/build/magician/coverage/evals/llm-observability-phase*/latest/`.

## Identity and correlation

Every new direct or queued request carries a provider-independent
`LlmTraceContext`. It distinguishes the root `trace_id`, one logical
`llm_call_id`, deterministic provider attempts (`{llm_call_id}:a{n}`), the
queue's `dispatch_job_id`, authoritative principal/workspace scope, and the
available task, execution, plan, step, iteration, chat-turn, parent, retry and
workload lineage. Provider retries retain the logical call id. Caller retries
receive a new call id under a shared retry group. Idempotent subscribers retain
the owner job/call identity and are explicitly marked as reused. The physical
attempt counter advances immediately before every provider invocation, so a
fallback-profile traversal yields `a1`, then `a2` even when retry policy sees
one outer routed request. Successes and terminal errors carry the concrete
effective profile/provider/model. If fallback selection or preflight fails
after an earlier provider invocation, that error retains the last physical
route rather than becoming an unattributed configuration failure. Queue
health/cooldown accounting follows the effective terminal provider, not merely
the operation's initial provider.

Chat tool-loop calls share the user turn as their root trace while retaining
distinct call ids. Agentic decision calls share the root execution and carry a
stable iteration id. For chat-created tasks, the agentic convergence point
hydrates `chat_session_id` from the task manifest (never from an execution-id
pattern), and preserves it in the model task reference, decision and local-prep
events, memory-utility review, event correlation, and pause/resume state.
Plan-mode task creation writes the originating session into that manifest as
well. Runtime-owned tool dispatch carries the session through native,
flat-loop and compiled-provider contexts—including coding child telemetry—and
does not trust model-supplied hidden provenance. Autonomous tasks keep that
field empty.

**Turn lineage is stamped at the source.** Every fact a turn produces — the
logical call, its provider attempts, and each tool execution — is stamped from
one `LlmTraceContext`. Anything left unset there is unset on all of them.
An inline chat turn has no task execution of its own, so the chat turn id **is**
its execution identity. `chat_turn_trace_context` sets `execution_id =
chat_turn_id` (regression-tested). A producer that populates lineage only on
the `LLMResponseReceived` event, never on the context, will fail the read
service's cross-dataset guard (tool rows and their owning call must agree on
every shared context column).

The cancellable streaming-chat boundary mints its call context and owns the
physical-attempt counter before awaiting the provider. If the future is
dropped, cancellation closes that exact logical call instead of losing the
receipt. An attempted failure may use only the effective route stamped on the
typed terminal error; the configured profile remains a requested-route hint
for zero-attempt/preflight failures and is never substituted after fallback.

Realtime voice mints one correlation identity and an exact wall-clock start at
the response boundary, before provider commit/injection wherever Magician owns
that boundary, and consumes the pair at terminal usage. A new utterance resets
both monotonic timing and correlation. Meeting responses use the same
per-response identity even though their upstream session is reused. Missing
detailed usage remains unknown; coarse totals stay coarse, and timeout/empty-audio
responses are failed calls rather than zero-cost successes. A backend realtime
provider error closes the active call with its original identity, exact start
and a content-free error class; session/configuration errors outside an active
response do not inflate failed-call counts. Logical chunk map, repair,
fallback and reduce calls are typed children of the aggregate logical request
(`chunk_map` / `chunk_repair` / `chunk_fallback` / `chunk_reduce`). Scope and
product lineage stay local and are never serialized into provider request
bodies.

Both the backend-proxied and browser-direct OpenAI adapters accept a detailed
realtime usage split only when every total, text/audio and cached bucket is a
non-negative integer, cached buckets fit their modality totals, and modality
sums equal the provider's coarse totals. Missing or inconsistent detail is not
coerced to zero: the valid coarse totals remain captured with unknown pricing.

Browser-direct WebRTC follows the same lifecycle. Completed responses always
close the call even when the provider omits usage, while failed, incomplete,
cancelled, interrupted, barge-in and session-close paths emit a bounded failure
terminal. Raw provider error text stays in local diagnostics only. Hands-free
voice is excluded from this realtime ledger because its billable model request
is the separately captured cascaded chat call.

Both `llm_calls` and `llm_dispatch` Parquet rows persist these identity
columns. Dispatch rows are partitioned by their typed scope rather than a
global default; the stable DuckDB `llm_calls` view continues to read historical
partitions with `union_by_name` and typed compatibility defaults. New
compatibility rows never persist provider reasoning or free-form errors:
reasoning is omitted, failures use a fixed presence marker, and malformed
response categories are replaced by a fixed machine category. Compatibility
defaults fill only physically absent historical columns. Existing SQL `NULL`
values remain unknown rather than becoming zero tokens/cost or a fabricated
route category.

`make test-llm-trace-phase1` is the provider-free invariant matrix.
`make llm-trace-phase1-audit` is the read-only historical/live join audit; a
runtime without Phase 1 data reports `no_phase1_data` rather than fabricating
coverage.

## Canonical recorder, journal, and materializer

`LlmTraceRecorder` accepts validated call-start, provider-attempt,
call-completion and capture-gap records. Call start and completion are
revisions 1 and 2 of one stable `call_fact`; attempt start, first-token and
completion are revisions 1, 2 and 3 of one stable `provider_attempt`. The
resulting `(record_kind, stable_id, revision)` key is the journal and
materializer idempotency boundary.

The contract includes decomposed timing, normalized token classes, versioned
pricing source, finish/refusal/truncation state, immediate validation and
explicit capture status. It contains no prompt, response, tool argument,
attachment or transcript content. Every typed record validates scope,
event/observation ordering, attempt identity, lifecycle completeness, cost
sanity and capture/training posture before a sink may accept it. Journal
envelopes add a monotonic sequence and BLAKE3 payload checksum. Each
serialized fact is capped at 16 KiB. Operation, capability and routing
categories are bounded machine tokens. Token validation also owns the
folded-bucket invariants used by pricing: cache-read plus cache-creation
cannot exceed input; a realtime modality split must contain every audio and
folded bucket; uncached audio plus all cache cannot exceed folded input; and
realtime rows cannot claim an unrepresentable cache-creation bucket.

Three independent bounded lanes reserve capacity for critical facts,
lineage/outcomes and restricted payloads; all currently defined fact records
are critical. Configuration rejects a restricted-payload lane larger than the
critical-fact lane. The hot path uses nonblocking enqueue only. A dedicated
blocking worker drains critical records first, appends checksummed JSONL
envelopes to rolling fsynced per-scope segments, then asks the materializer
to apply bounded batches from the uncommitted prefix. Its checksum-bound
watermark advances atomically only after each batch materializes successfully.
Accepted records leave the worker buffer as soon as the journal append is
durable; publication failures keep their backlog on disk.

Transport completion and caller validation are separate facts. A normal text
response leaves validation unattempted; structured callers emit an explicit
typed success only after their parser/schema/contract accepts the response, or
a typed failure when it rejects it. Validation errors stored in canonical
facts contain only a normalized class and a fixed content-free message.

Canonical recovery uses a rebuildable `recovery-index.sqlite3` beside the
journal segments. It stores record identities/checksums and byte locations,
call/attempt lifecycle state, and parent edges. It stores no prompt or response
payload. Index transactions commit after the corresponding journal bytes and
segment directory are fsynced. Losing an index transaction therefore leaves a
replayable journal suffix.

The first start with an existing journal streams and validates historical
records once, checkpointing each batch. Subsequent starts check segment
metadata, the indexed boundary and the materialization watermark, then decode
only the unindexed suffix. Changed sealed segments, missing indexed files,
invalid checkpoints and SQLite corruption fail closed. An index is derived:
a stopped-owner repair can preserve/rename the index and its SQLite sidecars
and rebuild it from the unchanged journal. Do not delete the canonical journal
or report Parquet files to repair an index. Copying/restoring a journal onto
another filesystem also requires rebuilding its local metadata-bound index.

Recovery and canonical publication batches hold at most 256 records and 8 MiB
of serialized journal data. Each open SQLite index has a 1 MiB page-cache
budget, memory mapping disabled, and disk-backed temporary storage. The store
retains at most eight open scope indexes; historical envelopes and lifecycle
maps are never cached in the canonical store. Related attempts and children
are streamed from indexed lookups, including late parent revisions. A partial
final JSONL write is repaired; malformed complete rows, sequence gaps,
checksum mismatches and conflicting deliveries fail closed. Same-checksum redelivery is deduplicated. Critical-channel
saturation increments exact principal/workspace/operation/reason gap groups;
the next recovered slot or bounded shutdown emits a typed `capture_gap` record.
Shutdown stops new acceptance, drains lanes in priority order, persists pending
gap facts, and reports any records or missing-count evidence left after its
deadline. Dropping the pipeline owner without the explicit async shutdown
handshake also closes admission under the recorder gate; the disconnected
worker then defensively drains and flushes the stable accepted prefix.

Only one process may own the journal writer lease. Startup waits for writer
index validation, and never abandons a still-running blocking writer after a
synthetic timeout. Historical Parquet catch-up runs after writer readiness;
explicit flush/shutdown barriers drain publication in bounded batches.
Existing date-partitioned reports remain queryable. Read envelopes mark
analytics stale and include a catch-up warning when indexed journal revisions
are ahead of the publication watermark. Segment configuration is capped at
64 MiB and individual line reads are bounded before decoding.

Tests: `make test-llm-trace-phase2b`. Index migration cost is paid once, on
first start with an existing journal.

Each journaled call lifecycle revision is written beneath
`analytics/llm_calls/dt=YYYY-MM-DD`, each provider-attempt revision beneath
`analytics/llm_provider_attempts/dt=YYYY-MM-DD`, and explicit loss evidence
beneath `analytics/llm_capture_gaps/dt=YYYY-MM-DD`. Partition dates use UTC
event time. A deterministic object name derived from the stable revision key
means replay never creates a second raw fact.

The complete batch is validated before publication starts. Each row is
projected through an explicit typed, content-free schema into temporary
Parquet, fsynced, atomically renamed, directory-synced and read back to verify
its identity and payload checksum. Only then does the dataset's atomic
checksum watermark advance. If a crash lands between object publication and
watermark advancement, replay verifies the existing object and repairs the
watermark. A same-key/different-checksum object fails closed.

Raw lifecycle revisions remain separate one-row Parquet objects so no facts
are overwritten. Compaction does not delete or mutate them.

## Governed reads

Completed UTC partitions can be compacted after a configurable raw file
threshold (64 by default; the current partition is skipped). Before
publication, the compactor validates a single record kind and scope, unique
revision keys and row-count parity. It fsyncs and atomically publishes the
Parquet object first, then atomically publishes a manifest containing every
sorted raw filename, byte length and BLAKE3 checksum plus the compacted output
checksum and journal range. A governed read chooses either that one compacted
object or the raw source set for a partition, never both. Missing, corrupt or
stale manifests and any changed raw source set fall back to raw revisions.

`LlmAnalyticsReadService` owns the exact principal/workspace path boundary,
stable schema, latest-revision coalescing, terminal-attempt enrichment, typed
filters, sorting, pagination, totals and overview formulas. Its content-free
registry allowlists `llm_calls`, `llm_provider_attempts`, `llm_capture_gaps`,
`llm_call_revisions` and `llm_provider_attempt_revisions`; relation and column
names must match exactly. Historical `batch_*.parquet` call rows remain visible
through typed defaults, carry an explicit `legacy_schema` training exclusion,
and cannot override the trusted path scope. Empty datasets expose the same
typed schema instead of failing view construction. Typed reads default to a
24-hour half-open range, reject ranges over 31 days, and discover only UTC
partitions intersecting that effective range before opening Parquet. Governed
source installation and schema validation run under the same connection
interrupt budget as consumer queries. Negative count/token aggregates fail
closed instead of being coerced to a plausible zero.

Governed-read failures map as: a contended DuckDB guard
(`LLM_READ_GUARD_TIMEOUT_MESSAGE`) is `503` with `Retry-After`; every other
reader failure is `500`. `400` stays reserved for genuine client faults —
missing/unsafe scope and unsafe legacy SQL.

REST routes under `/api/magician/v2/analytics/llm/` expose overview, catalog/
schema, paginated logical calls and provider attempts, single-record details
and a bounded fact query:

- `GET /analytics/llm/overview`
- `GET /analytics/llm/traces`
- `GET /analytics/llm/traces/{trace_id}`
- `POST /analytics/llm/content/grants`
- `GET` + `DELETE /analytics/llm/content/calls/{id}` (grant via
  `X-Magician-Content-Grant`)

The `/llm` page's canonical capture-health band consumes the overview envelope
and shows logical calls separately from attempts, queue/local/provider/TTFT/
validation/end-to-end timing, contract-validation coverage, freshness and
explicit capture gaps. Telemetry coverage compares captured canonical call and
attempt revisions with the recorder's exact missing-record counts. Cost totals
sum the known subset, are labeled USD, and carry the observed pricing versions
and cost sources. Separate `usage_observed_calls` and `cost_observed_calls`
denominators preserve missing provider usage/pricing versus a legitimate
reported zero. Crew-health rolling spend follows the same rule.

`internal_data` actions: `llm_observability_overview`, `list_llm_calls`,
`read_llm_call`, `list_llm_provider_attempts`, `read_llm_provider_attempt`,
`query_llm_facts`, `list_llm_traces`, `read_llm_trace`. `read_llm_call_content`
is deliberately absent. Every action returns the shared
scope/range/filter/freshness/coverage/pagination/warnings/data envelope.
Runtime-injected `__principal` and `__workspace` are mandatory and
authoritative; public principal/workspace values may only assert the same
scope.

The fact-query escape hatch is not a token-scanned SQL path. It parses one
`SELECT`/`WITH` statement into an AST, permits only the content-free fact
registry and lexically scoped local CTE names, rejects fact-relation
shadowing, non-recursive self references, qualified relations and table
functions, and rejects query-AST side effects such as `SELECT INTO` plus
non-SELECT bodies at every outer, CTE, derived-table and expression-subquery
node. It disables DuckDB external access after governed source tables are
installed. It retains the same 31-day, 1,000-row, interrupt and serialized-byte
bounds as typed reads. Aggregate SQL does not invent a total: it omits
pagination and reports a limit probe as explicit censoring plus a warning.
The complete single-query or batch response is bounded to four MiB.

Legacy `query_llm_calls` remains for compatibility, with its own AST
relation allowlist and content-free projection across REST and `internal_data`.
Only `llm_calls`, `chat_session_cache_summary`, and lexically valid CTEs are
visible. Caller CTEs cannot use the server-private `__llm_fact_bounded_*`
namespace. Overview counts, tokens, costs and timing averages fail closed when
DuckDB returns negative or non-finite values.

## Activation and reconciliation

Canonical capture subscribes before normal runtime workers begin emitting model
responses; the compatibility mirror subscribes at that same boot
boundary. For each scoped `LLMResponseReceived`, the bridge records stable
call-start, final-provider-attempt and call-completion revisions through the
typed recorder. Agentic calls retain the canonical logical `iteration_id`
unchanged and record provider conversation projection separately as
`prompt_projection_mode` (`bootstrap`, `continuation`, or `rebootstrap`).
The mode is stamped before response lowering.

The bridge does not infer missing telemetry. Missing scope, typed correlation,
attempt count, attempt id or a valid absolute start timestamp produces a
granular `runtime_event_mapping_rejected` gap; completion minus latency is not
used to invent a start. If a final receipt reports multiple provider attempts
but historical attempt lifecycle events are unavailable, only the final
observed attempt is materialized and the exact earlier count is recorded as
`earlier_provider_attempt_lifecycle_unavailable`. Invalid TTFT or cost values
are reported, but a valid provider-usage payload is repriced canonically from
its token buckets and the effective-dated pricing table rather than trusting
the producer's dollar assertion. A stale producer amount emits
`producer_cost_mismatch_recomputed`; impossible cache/reasoning/realtime bucket
relationships are omitted with `invalid_provider_usage_omitted`. Realtime
reconstruction removes all cache before deriving uncached text. Provider
adapter aliases are normalized by an explicit finite set. An idempotent queue
subscriber marked `response_reused` is counted by activation telemetry but
does not create a second logical-call or provider-attempt lifecycle.

If a successful compatibility producer reports a stable call receipt but no
effective provider/model, the bridge preserves the logical completion and
provider-reported usage, leaves pricing unknown, and emits a call-owned
`terminal_provider_attempt_lifecycle_unavailable` count. It never promotes the
requested/configured profile into a supposedly observed physical route.

The historical `analytics reprice-llm-calls` correction path applies the same
bucket arithmetic. It skips unknown pricing routes, invalid/missing token or
timestamp values, and coarse realtime rows without a complete modality split.

Every persisted response, validation and mapping-gap category is a bounded
machine token. Mapping diagnostics are collapsed through a finite reason-code
table; arbitrary diagnostic prose maps to `mapping_validation_failed`.

Failed calls follow the same non-invention rule: when the queue or direct-route
error carries an exact effective profile/provider/model, the bridge
materializes that known final failed attempt with unknown usage and pricing,
and gaps only preceding attempts.

Broadcast lag is a special diagnostic class because the shared transport bus
cannot reveal the types of skipped envelopes. The exact skipped-envelope count
is exposed as `unclassified_transport_events_lost`, excluded from the
known-LLM fact coverage denominator, and treated as a strict live-audit
failure. The process-level diagnostic is stored under `anonymous/default`.

The coverage ratio is revision-based. `known_missing_fact_revisions` includes
only critical buffer/worker drops and unavailable earlier provider attempts.
Mapping rejection and invalid-field diagnostics remain visible in
`capture_gaps` but do not distort the captured-revision denominator.

At startup, existing scoped journal indexes are recovered before new commits;
historical analytics publication catches up independently.
Duplicate delivery is harmless only when the stable revision checksum agrees;
a conflicting payload fails closed. Shutdown first cancels and joins background
response producers, then drains queued response events, including lag
accounting, performs the bounded durable drain and explicitly flushes the
temporary compatibility mirror's final batch.

The legacy `llm_calls` sink remains a best-effort compatibility mirror, not a
completeness authority. It ignores request-start events because the canonical
lifecycle owns starts, attempts, completion and gap evidence. The stable
`llm_calls` view hides a mirror row when its `llm_call_id` is already
canonical. Scoped `llm_dispatch` remains the auxiliary source for queue wait,
local-prep and provider-execution timing. Governed reads join those fields to
canonical attempts by both `dispatch_job_id` and `llm_call_id`. The 90-day UTC
retention sweep covers canonical calls, attempts, capture gaps and
legacy/event partitions. Journal segments are excluded because only journal
compaction may remove replay state.

**A retention horizon of zero is rejected, and the sweeper skips it anyway.**
The restricted-content sweeper is spawned unconditionally at boot and re-reads
`analytics.llm_trace.retention` every hour, so it runs whether or not tracing
is enabled — and with a zero horizon the cutoff is today, which matches every
partition ever written. Two of the four datasets it sweeps have no other copy:
`llm_content_tombstones` and `llm_content_access_audit`.

Validation therefore rejects a zero `facts_days`/`context_metadata_days`
regardless of `enabled`. The sweeper independently treats zero as *not
configured* and returns without deleting. A sweep that removes anything logs
what it removed. The singular storage-maintenance runtime owns that sweep,
completed-day batch compaction, and five-minute rolling compaction of active
canonical LLM partitions. Governed reads use the verified compacted prefix
plus a bounded raw tail. Shutdown cancels and joins maintenance after response
producers quiesce and before the canonical and compatibility pipelines publish
their final batches.

`make llm-trace-phase2f-audit` compares canonical and compatibility rows by
`llm_call_id`, requires exact token/latency parity, bounded floating cost
parity, pricing provenance, complete captured-plus-gap attempt accounting, at
least 99% stable queued-timing joins, a consistent governed overview, and
bounded content-free response/category fields. Attempt accounting is proven
independently for every logical call: each call-specific unavailable-attempt
gap carries its owning `llm_call_id`, while process-wide lag and saturation
gaps remain unowned.

## Sanitized and restricted content

`magicllm` emits borrowed, in-process observations for the logical request,
each effective provider request and each normalized successful response.
Magician applies the active policy, clones an eligible observation into a
record- and 64-MiB-byte-bounded memory lane and sanitizes it on a dedicated
worker before any journal or Parquet write. The observer handle and one-shot
logical-emission marker are excluded from request serialization.

The default remains `content_mode: metadata`. Exact scope overrides win over
exact operation overrides, then the global policy applies. Sanitized sampling
is stable by call and operation. The default public-guest exclusion guard keeps
anonymous content metadata-only. Enabling training eligibility without
sanitized capture, zero sanitized retention, fail-open redaction or encrypted
raw-local capture is a configuration error.

The sanitizer uses the scope's current secret-store values as redaction
canaries and catches private keys, bearer/credential forms and common token
shapes. Sensitive structured values, dynamic object keys and tool identifiers
are treated as untrusted content. Binary/media bytes and URLs become reference
metadata plus a per-scope keyed fingerprint. Reasoning keeps only its character
count and keyed fingerprint; provider raw bodies and raw response ids are never
captured. Payloads beyond the configured bound become explicit
`content_omitted` metadata. Any processing failure degrades to one fixed,
content-free capture-gap category.

Sanitized I/O and context descriptors are stored separately from the canonical
fact catalog under `analytics/llm_call_io`, `analytics/llm_context_blocks`,
`analytics/llm_content_tombstones`, and `analytics/llm_content_access_audit`.
These datasets use an independent restricted journal, writer lease, replay
watermark and retention policy. They are not registered in fact SQL or exposed
through `internal_data`. Committed payload journal segments are pruned only
after verified Parquet publication.

The restricted REST flow first issues a setup-token-authenticated, one-use
grant at `POST /api/magician/v2/analytics/llm/content/grants`. A caller then
supplies that grant only through `X-Magician-Content-Grant` to
`GET /api/magician/v2/analytics/llm/content/calls/{llm_call_id}`. The service
binds scope, actor, target, reason, optional execution and expiry server-side,
re-sanitizes with current policy, caps one reveal at 256 revisions/one MiB and
fsyncs a restricted audit marker before returning it, then queues the governed
Parquet audit copy. Successes and failures are `no-store`; failure bodies
contain only fixed machine codes. `read_llm_call_content` remains absent until
a typed trusted grant can reach provider dispatch without model arguments
becoming authority.

Setup-token-authenticated deletion uses
`DELETE /api/magician/v2/analytics/llm/content/calls/{llm_call_id}`. An atomic,
fsynced revocation marker becomes the immediate and restart-safe denial
authority before the append-only analytics tombstone is queued. Configuration
and the sweep both ensure tombstones cannot expire before the sanitized I/O
they protect.

`make llm-trace-phase3-audit` never stores or prints payload excerpts. Set
`LLM_TRACE_PHASE3_ARGS` to select a non-guest cohort or one real call. The
optional restricted API probe requires both `--call-id` and a valid
`MAGICIAN_SETUP_TOKEN`. A metadata-only runtime is reported as skipped rather
than as false success.

## Tool, action and branch lineage

Phase 4 adds one content-free, append-only tool lifecycle keyed by the
provider's normalized tool-call id and its producing `llm_call_id`. The stable
execution edge is `llm_call_id:tool:model_tool_call_id`; the model's arguments
are canonicalized and stored only as a scope-keyed HMAC. Full arguments, tool
results, delegated content and prompts never enter `llm_tool_calls`.

Each lifecycle can record proposal, name recognition, argument parsing, schema
validation, authorization, approval, execution start/finish, result validation,
one or more later LLM consumers, branch materialization and rollback. Tool
execution success and result-contract validity are independent. Failure owner,
side-effect state, successful-path membership, same-argument repeat count and
observation/action cycle count are bounded machine facts. Authorization records
distinguish approval being required from approval actually being obtained.
Admission failures never manufacture execution or result stages. Once a
transport ran, failure, cancellation and timeout leave side effects `unknown`;
only an authoritative rollback can move that state to `reversed` or
`rollback_failed`. Known file-edit rollback is emitted only from exact
runtime-authored snapshot-restoration markers.

Foreground Chat and agentic execution use the same lineage emitter. Agentic
coverage includes compiled/MCP tools, browser and screen actions, delegation,
spawned subgoals and the external coding engine at its Magician boundary.
Delegation stores bounded related execution ids, never delegated payloads.
When fingerprinting, event mapping or the bounded lineage buffer fails, the
system emits an explicit content-free linkage/capture gap. Lineage-buffer gaps
retain their owning call id. Recovery is attributed only after an earlier
authoritative transport failure, and every later append-only LLM call that
receives a retained tool result gets its own consumption edge. Branch success
here means that the local tool/action branch materialized successfully; later
outcome phases attribute that branch to the task's terminal outcome.

The governed `llm_tool_calls` relation preserves the ordered lifecycle rather
than applying latest-wins coalescing. `GET /analytics/llm/traces`,
`GET /analytics/llm/traces/{trace_id}`, `list_llm_traces`, `read_llm_trace`,
and call detail all use the same scope-enforcing assembler. `/llm` includes a
call explorer with provider attempts and the ordered tool timeline.

`make test-llm-trace-phase4` is the provider-free matrix.
`make llm-trace-phase4-audit` is the bounded live exit audit (aggregate counts
and machine categories only — never stable ids or payload excerpts).
