# Storage Governance

The canonical owner list, class, tier, and readiness ledger is
[`storage-catalog.yaml`](storage-catalog.yaml), documented in
[Storage Abstraction](storage-abstraction.md). `/storage` is the operator
projection of that catalog (`governance_id`). Drift between the two fails
`make check-storage-catalog`. This page describes the live `/storage` surface,
not a second inventory.

Magician owns several kinds of local state. They do not share one safe cleanup
rule: mail reconciliation state is lifecycle-authoritative, thread tombstones
preserve identity, DuckDB files can contain physical churn, telemetry is
time-bounded, and the LLM recovery journal must outlive its derived Parquet
facts. Storage governance makes those distinctions executable and visible.

The operator surface is `/storage` (also available from Settings and the
command palette). It is scope-aware and reports apparent size, allocated disk
blocks, WAL size, file count, approximate DuckDB rows, partition range,
retention, safety class, inventory completeness, and the actions permitted for
each store.

Track B activation evidence is `GET /api/magician/v2/storage/activation` and
`magician storage status` (readiness-qualified). The panel shows remaining
blocks and offers no cutover action. The Gate 3 drills live in
`magician/src/magician_v2/gate3/`; their budgets are remote-durable except lines
named `Local` (a local put pays a chain of full-device flushes rather than one
network round trip). Wall-clock budgets are enforced by
`make test-storage-budgets` on an idle machine; the saturated `make test-rust`
lane only records them. Gate closure is a compile-time assertion in
`magician-storage/src/gate3.rs`, so reopening it breaks the build. See
[adr-2026-09-01-performance-capacity](../magician-storage/adr-2026-09-01-performance-capacity.md).
Switching the desktop Runtime provider (`local_file` / `silverbullet_space`)
does not move data. New implicit-path bypasses fail
`make check-typed-storage-boundaries`. Reaching `remote_ready` does not delete
the local source.

## Ownership and lifecycle

| Store | Class | Automated lifecycle | Operator action |
| --- | --- | --- | --- |
| `analytics/analytics.duckdb` | Regenerable catalog | Store-owned checkpoints | Verified copy-compaction |
| `channel_assist/channel_assist.duckdb` | Lifecycle-managed comms state | Provider reconciliation only; no generic age deletion | Verified copy-compaction only |
| `ui_threads/ui_threads.duckdb` | Authoritative thread index | Mutations checkpoint at most once per 30 seconds | Verified copy-compaction preserving soft-delete tombstones |
| `feed/feed.duckdb` | Regenerable projection | Source-owned refresh | Purge only rows proven orphaned against the authoritative task list |
| `social/social.db` (`social_sqlite`) | Lifecycle-managed SQLite | Scope-isolated social network; copy-compaction via VACUUM | Compact safely |
| `attention_funnel.db` | Lifecycle-managed SQLite | 30 days and per-scope cap | Inspect only |
| `resurfacing.db` | Lifecycle-managed SQLite | 90 days and per-scope caps | Inspect only |
| `attention_learning.db` | Shared physical, scope-owned learning ledger | Explicit 30–3650 day scoped retention; active evidence and referenced decisions remain protected | Optimize online / preview and clean old scoped history / verified physical rebuild |
| `analytics/browser_engine_usage.sqlite3` | Scope-owned observability SQLite | Sanitized command-attempt rows; 90 days and 50,000 rows per scope | Inspect through Observe → Stats |
| `apps/app_store.sqlite3` | Authoritative encrypted App registry, records, grants and receipts | Created lazily by its authenticated App owner; migrations, retention and purge remain App lifecycle operations | Owner-coordinated integrity check, checkpoint/optimize and encrypted compaction |
| `apps/packages` | Lifecycle-managed immutable package bytes | Descriptor-pinned staging, create-only atomic promotion, bounded authenticated stale-staging recovery; exact bytes revalidated before registry publication; import adds bytes only after bounded authenticated archive admission; retention owns final-byte retirement | Inspect only |
| `apps/attachments` | Dormant restricted retained attachments | Future app retention/purge settlement only | Inspect only |
| `apps/exports` | Dormant restricted export archives | Future archive, encryption and purge owners only | Inspect only |
| `apps/captures` | Dormant restricted prompt/debug/provider captures | Future data-policy and retention owner only | Inspect only |
| `apps/evaluations` | Dormant restricted evaluation artifacts | Future data-policy and purge settlement only | Inspect only |
| `events`, `memory_events`, legacy `llm_calls`, `llm_embeddings`, `llm_dispatch` | Observability Parquet | Completed-day compaction; 90-day retention | Compact completed days / apply retention |
| `analytics/activity_rows` | Observability Parquet — the activity spine, one row per completed span | Two-level partitions (`dt=…/hour=…`); closed hours fold after 2 hours and closed days after 48 hours; **7-day** detail retention, rolled up rather than deleted | Compact partitions; Apply retention runs the tiered sweep (the 7-day window is not expressible as the action's `retention_days`) |
| `analytics/activity_rollups` | Observability Parquet — per-hour summaries of expired spine detail | One row per `(dt, hour, kind, workload_class, agent_id, outcome)` with count, p50, p95 and total duration; **13-month** retention | Expired by Apply retention's tiered sweep; nothing to fold, since maintenance produces one object per day |
| `events.jsonl` | Per-scope append-only transport log | Bounded by the transport-log compactor to a 24-hour / 2,000-event window per scope; the inventory resolves its path through the compactor's own scope sanitizer, which is stricter than the workspace layout's | Inspect only |
| Canonical LLM calls, attempts, tool calls and capture gaps | Governed observability facts | Rolling active-day and final closed-day compaction plus 90-day retention | Compact analytics / apply retention; canonical immutable revisions remain the corruption fallback during compaction |
| Restricted LLM content/context, tombstones and access audit | Restricted | Live configured restricted-content/fact retention | Inspect live policy and size |
| LLM trace and restricted-content journals | Authoritative/restricted recovery state | Journal-owned committed-segment lifecycle only | No generic destructive action |
| `storage_governance/compaction_metrics.json` | Bounded, content-free observability | Atomic ledger capped at 256 effective compaction events per scope | Inspect impact / clear metrics only |

The inventory is read-only. Asking for an app path or loading `/storage` does
not create `apps/`, open SQLite, stage packages, or enable an app route; only an
authenticated registry/lifecycle write, package-stage/recovery operation or
package import materializes its owned paths (import never creates the registry,
and package export refuses to create an absent tree). Those writers share an
exact canonical `apps/scope-binding.json`, so two principal/workspace values that
normalize to the same filesystem names cannot share state. Missing paths appear
as zero-byte entries. The inventory counts SQLite `-wal` companions, rejects
symlinked path components and skips child symlinks. Walks are bounded (app
directories 20,000 entries / 64 levels; others 50,000 / 64); hitting a ceiling
marks the entry and aggregate as a **partial** inventory rather than presenting
an undercount as complete. The app entries expose no generic maintenance action.

Snapshot requests use bounded scope-keyed gates shared with maintenance plus a
two-second per-scope cache (capped at 256 scopes, invalidated by every
maintenance mutation), so concurrent refreshes coalesce into one walk without
serializing all scopes.

### What the inventory measures, and where

- **Path resolution follows the writer, not the layout.** The per-scope
  transport log is written through a sanitizer that accepts only
  `[A-Za-z0-9_-]` up to 128 characters and routes everything else to
  `_quarantine/_quarantine`, while `ArtifactV2Workspace` accepts any segment
  that is not a traversal. For a principal like `user.name` the two disagree.
  The inventory displays the path base-relative so the row names the directory
  the bytes are in. The quarantine warning is emitted once per distinct scope
  from the sanitizer itself. The set of reported pairs is capped and collapses
  into a single "further routing will not be reported" line at the ceiling.
  Each half of the key is capped at 128 bytes, the same ceiling
  `is_safe_scope_component` applies.
- **Write-ahead sidecars are only looked up where one exists.** SQLite has
  `<file>-wal`, DuckDB has `<file>.wal`, and a journal has neither — its
  durable writer stages through a uniquely-named temp and renames.
- **`retention_days` carries the window the policy prose states** wherever a
  real one exists — the transport log's rolling day, the attention funnel's 30
  and resurfacing's 90. `None` means no age-based window. The transport log's
  value is derived from the compactor's own `EVENTS_RETENTION_WINDOW_MS` and
  is rounded **up** with a floor of one day, so a sub-day window never reports
  `0`.

## Attention-learning maintenance

The Attention Learning card deliberately exposes three separate operations;
none is presented as doing the work of another:

- **Optimize now** runs `ANALYZE` on the serving-hot decision, impression and
  canonical-membership tables followed by SQLite `PRAGMA optimize`. It runs
  online, preserves every logical row, and refreshes planner statistics. It is
  not a disk-reclamation operation and may make a small statistics write.
- **Clean old history** first performs a read-only preview for the selected
  principal/workspace and retention window. Apply requires a second explicit
  confirmation and re-evaluates the same store-owned retention rules; active
  evidence, current models, idempotency anchors and referenced decisions or
  projections remain protected. The cleanup removes logical rows but does not
  promise a smaller SQLite file.
- **Reclaim disk space** preserves all remaining logical rows. It holds the
  in-process writer and an exclusive cross-process lifecycle lease while
  SQLite builds a sibling `VACUUM INTO` candidate, verifies
  integrity, foreign keys, schema, file identity pragmas, AUTOINCREMENT state,
  and every table's exact row count, then fsyncs the candidate. Ordinary reads
  remain available during that long build. Only
  the final WAL checkpoint and atomic replacement enter a bounded fail-fast
  read-drain window. A hard-link rollback copy is retained until the installed
  writer and read pool reopen successfully. This action is intentionally
  global because all scopes share one physical file. A second live Magician
  process or concurrent reclaimer causes a bounded conflict instead of an
  unsafe swap across still-open SQLite handles.

Automatic scoped retention has a size guard, and the guard must never be the
reason the store cannot shrink. Above `AUTO_SCOPED_RETENTION_MAX_DB_BYTES` the
scheduled pass narrows instead of skipping: it deletes only the first
`AUTO_SCOPED_RETENTION_OVERSIZED_SLICE_MS` (24 h) of history past the scope's
oldest retention anchor, so each pass is bounded work. An oversized store runs
on `AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS` (60 s) because a day retired
per day only keeps pace with accrual; a healthy store keeps the daily interval.
Pass scheduling rules:

- The gate records a pass's **finish**, refuses to start while one is in flight,
  and over the guard doubles the cadence for each consecutive pass that retired
  nothing, capped at the daily interval. It takes the fast interval as its floor
  before stat-ing the file; a store found healthy restores its previous gate
  timestamp so the daily interval can elapse.
- A productive pass over the guard is followed by at least
  `AUTO_SCOPED_RETENTION_WRITER_SHARE_FACTOR` (4) times its writer hold of quiet,
  so maintenance takes at most a fifth of the writer from page reads.
- The eligibility survey runs on a read connection; the writer is held only for
  the delete transaction.
- Anchors must be able to move: a terminal rank-recompute job retires with its
  outcome's time or its own, whichever is inside the window; outcomes retire
  before decision items; per-table report counts use the delete predicates.
- The three whole-table body sweeps (feature vectors, item revisions,
  diagnostic revisions) run only after a pass that retired rows. Their
  correlated lookups rely on `attention_history_item_feature_idx` and
  `attention_history_binding_feature_idx` (the latter also serves the FK check
  on vector delete); without them each is a whole-table scan per vector.
- A completed pass logs `retired_rows`, `empty_passes`, `elapsed_ms`.

Retention removes logical rows; **Reclaim disk space** returns the pages.

Serving does not count retained scope history on each request. A transactional
scope counter tracks verified impressions, while decision hydration is keyed
by the already-authorized decision id. Existing stores receive one durable
counter backfill during schema migration; triggers keep it exact afterwards,
including retention deletes.

The background storage owner runs a full sweep at startup and every six hours
across real scoped directories. A lightweight pass rolls active canonical LLM
partitions every five minutes once their uncompacted tail reaches 64 files. One
scope failure is isolated and logged; it does not prevent the remaining scopes
from being maintained. Restricted content retains its
configuration-owned lifecycle worker; embedding telemetry is governed by the
same singular storage owner as the other observability streams so pruning
cannot race verified compaction. Service shutdown explicitly cancels and joins
this owner after response producers quiesce and before the canonical and
compatibility LLM pipelines publish their final batches. An in-flight sweep is
therefore awaited, and no detached maintenance task can race the last durable
analytics writes.

## Governed reads must consume compaction output

Compaction only pays off if readers actually read its output. Canonical LLM
fact revisions (`part-call_fact-<hash>-rN.parquet`) are retained **on purpose**
as a corruption fallback, so they accumulate per call and are never pruned.

**Select sources through `llm_fact_compactor::governed_dataset_sources*`, never
by listing a partition directory.** Those helpers return
`[compacted] + uncompacted_tail` only after validating manifest scope, schema,
source-set checksum, that every compacted source still exists, the compacted
file's byte length and BLAKE3 checksum, and `source_row_count ==
compacted_row_count`; any mismatch falls back to the full raw set. That makes
the selection lossless by construction and fail-closed. The legacy batch stream
has the equivalent helper in `parquet_maintenance::partition_sources`. Reading
raw revisions is not a correctness bug (the de-duplicated relation still returns
one row per call), so the mistake shows only as latency.

## DuckDB copy-compaction

DuckDB `VACUUM` does not reclaim the physical file in the way operators expect,
so the three live DuckDB owners use a copy-and-swap protocol:

1. Hold the store's writer mutex and, where present, its cross-process writer
   lease; checkpoint the current file.
2. Fingerprint table DDL and exact row counts plus index, view, and sequence
   definition/current-value state.
3. `COPY FROM DATABASE` into a sibling file and fsync it.
4. Open the copy and require an exact fingerprint match.
5. Keep the source as a sibling rollback backup, publish the verified copy,
   run idempotent owner bootstrap/migrations, and verify it again.
6. Delete the backup only after the published connection is usable and its
   parent directory is durable.

If post-publication initialization or verification fails, the new file is
quarantined for the duration of rollback and the untouched original is restored
and reopened. Symlinks and special files are refused.

Mail compaction is deliberately physical only. It never deletes messages,
threads, annotations, feedback, reconciliation cursors, writing preferences,
or audit events. Mail deletion remains provider- and user-lifecycle-owned.

## Parquet generation protocol

Completed UTC partitions (never today's partition) are compacted when they have
at least two raw batches. The replacement is generation-named, for example:

```text
memory_events.compacted.01K....parquet
_compact/memory_events.compaction-manifest.json
```

The prior generation remains authoritative while the next generation is
written and verified. The manifest atomically selects the active generation
only after row-count, checksum, file-length, and fsync guards pass. Raw batches
are fingerprinted again immediately before deletion. A changed source, symlink,
row mismatch, malformed output, or lock timeout retains the source batches and
fails that partition closed.

Late batches remain visible alongside the manifest-selected generation until a
later sweep merges them. After the new manifest is durable, verified raw inputs
and inactive compacted generations are removed. This ordering prevents a crash
between object publication and manifest publication from hiding or replacing
the prior rows. If a manifest is later malformed or missing after raw pruning,
readers may recover only a sole valid compacted generation; multiple candidates
or selected-object checksum drift fail closed. The next guarded sweep republishes
the recovered manifest.

The shared 90-day observability lifecycle covers runtime events, memory events,
local embedding batches, legacy/canonical LLM calls, provider attempts,
tool-call lineage, capture gaps, and dispatch telemetry. A partition at the
cutoff date is retained. Generic
retention does not traverse mail, tasks, memory facts, SQLite lifecycle stores,
restricted-content stores, or the recovery journal.

Every step of a scope's sweep is independent. Compaction and retention are
separate obligations — compaction failing is a file count that stops improving,
retention failing is a disk that fills — so a failure in one is logged against
the partition or dataset that caused it and the rest of the sweep continues.

### The activity spine's two-level fold

`analytics/activity_rows` writes far more rows than anything else. It
partitions `dt=YYYY-MM-DD/hour=HH/` and folds twice: a closed hour after **two
hours** (on the five-minute lane), a closed day after **48 hours** (its `hour=`
directories are then removed). Both folds use the generation/manifest protocol
above, so a late row in a folded hour is merged by the next sweep.

Retention is **tiered**, not part of the shared 90-day sweep: full rows are kept
7 days, then summarised into `analytics/activity_rollups` (one row per `(dt,
hour, kind, workload_class, agent_id, outcome)` with count, p50, p95, total
duration) and only then removed. Rollups are kept 13 months. `count` and
`sum_duration_ms` are additive across objects; `p50_ms`/`p95_ms` are not — exact
percentiles over wider windows must come from detail.

**The marker is the commit record, on both sides.** After a rollup is published,
a `_rollup/rolled-up.json` marker is written inside the **rollup** partition
before the detail is deleted; a crash in between tells the next pass to finish
the delete rather than aggregate again. The marker records **which sources**
each rollup generation covers, so a batch landing in an already-rolled-up day
becomes an additional generation instead of being deleted uncounted; the fold
skips days that already have a rollup generation. No double count is possible
because:

- the marker lives in the rollup partition, removed only whole after 13 months;
- a rollup object's name is a hash of its source fingerprints, so a re-run
  renames over an interrupted run's object instead of adding one beside it;
- before publishing, every Parquet object the marker does not name is renamed
  out of the `.parquet` namespace, and readers select the marker's generations
  rather than listing the directory (listing is only the fallback for a
  partition with no valid marker).

**"Unaccounted" and "unreadable marker" are different states.** Quarantine runs
only where a generation is about to be published or superseded (never on the
two delete-without-aggregate paths: a legacy marker over an emptied day, or a
day with no recognised sources), and only with a manifest that genuinely
describes the partition — an uninterpretable marker refuses rather than reading
as empty, which would rename every committed generation away. The read side may
still collapse absent/uninterpretable to "list the directory": a wrong reader
misreports one query, a wrong writer deletes a day's summary.

**Rolling up is what authorises the delete.** The roll-up refuses — keeping the
detail at the cost of disk — when:

- the partition (day level or any `hour=` directory, since the delete is
  recursive) holds a Parquet object `partition_sources` cannot account for
  (it recognises `batch_*` files and the manifest's own generation;
  verified-but-unpruned `batch_*` files are excluded because their rows are in
  the compacted generation);
- only a legacy detail-partition `_rollup/rolled-up.json` vouches for the day
  (it names one object and no fingerprints; remove it to force one re-roll — a
  day holding only that marker directory is still deleted);
- the rollup marker exists but this build cannot interpret it (malformed JSON or
  a newer `schema_version`);
- any other summarise-and-verify error.

A day stuck refusing is an operator wedge; it escalates from `warn` to `error`
once overdue by more than the retention window, and never auto-gives-up.

### Rolling canonical LLM generations

Canonical LLM facts use a stricter append-only variant of the protocol. Their
raw revisions are never deleted by compaction. A versioned manifest selects a
checksum-verified compacted generation covering an explicit subset of those
immutable revisions; files appended afterwards are returned as a raw tail in
the same governed read. New observations therefore remain immediately visible
without invalidating the previous compacted generation.

At startup and every five minutes, the storage owner checks only the active UTC
partition. When its tail reaches 64 files, it merges the prior compacted object
and that tail into the next verified generation and atomically publishes a new
manifest. Governed queries consequently read one compacted file plus at most a
small between-sweep tail instead of reopening thousands of daily fragments.
Closed partitions are finalized by the full six-hour sweep. Version-1 exact-set
manifests remain readable as safe prefixes.

Readers verify the selected compacted object's length and checksum. Missing,
malformed, cross-scope, duplicate-key, or corrupt generations fail closed to
the immutable raw revisions. Each less-frequent generation roll also rechecks
all covered raw fingerprints; ordinary reads do not pay that thousands-of-files
checksum cost. Publication still uses a temporary Parquet file,
row/idempotency/scope/sequence validation, fsync, atomic rename, and an atomic
manifest write; an append racing compaction simply becomes part of the next
tail.

## Compaction impact metrics

Every effective manual, startup, six-hour, and five-minute compaction records a
content-free event in the active scope. Database events identify the owning
DuckDB. Parquet and canonical LLM events identify the dataset and record the
partitions, source files and rows folded, apparent bytes before/after and
reclaimed, duration, and trigger. Canonical LLM metrics additionally separate
`query_files_avoided` from file-size reclamation: immutable source revisions are
retained as the corruption fallback, so the important win is often reduced
read fan-out rather than less disk usage. The size of newly written compacted
objects is reported separately and is not presented as reclaimed space.

The ledger is one atomically replaced JSON file, retains at most 256 events,
and exposes only its 24 most recent events in the snapshot. No-op scheduled
sweeps do not add records. A malformed or unsupported ledger is reported as
unhealthy instead of being trusted; the clear action resets it. Failure to
write this auxiliary ledger logs a warning but never converts an already
verified data compaction into a failed operation.

`/storage` shows retained-history reclaimed space, compacted files, query files
avoided, per-area totals, recent runs, and the metrics ledger's own apparent
and allocated footprint. **Clear metrics** requires an explicit confirmation
and deletes only this ledger. It cannot traverse or delete databases, Parquet
objects, manifests, journals, mail, tasks, memory, or any other scope data.

## HTTP contract

All routes require a workspace-bound bearer. The server resolves both
principal and workspace from that credential.

| Method and route | Purpose | Required body confirmation |
| --- | --- | --- |
| `GET /api/magician/v2/storage` | Typed inventory and permitted actions | n/a |
| `POST /api/magician/v2/storage/actions/compact-databases` | Compact selected `analytics`, `channel_assist`, or `ui_threads` owners | `COMPACT DATABASE` |
| `POST /api/magician/v2/storage/actions/compact-parquet` | Compact completed generic and canonical LLM partitions | `COMPACT PARQUET` |
| `POST /api/magician/v2/storage/actions/apply-retention` | Apply a scope-bound 30–3650 day observability window, plus the activity spine's own 7-day/13-month tiered sweep | `APPLY RETENTION` |
| `POST /api/magician/v2/storage/actions/clear-compaction-metrics` | Clear only the bounded metrics ledger for the explicit scope | `CLEAR COMPACTION METRICS` |
| `POST /api/magician/v2/storage/actions/attention-learning/optimize` | Refresh attention-learning planner statistics online | `OPTIMIZE ATTENTION` |
| `POST /api/magician/v2/storage/actions/attention-learning/retention-preview` | Preview scoped logical cleanup; `retention_days` must be 30–3650 | none; confirmation is rejected |
| `POST /api/magician/v2/storage/actions/attention-learning/retention-apply` | Apply the previewed class of scoped retention rules | `CLEAN ATTENTION HISTORY` |
| `POST /api/magician/v2/storage/actions/attention-learning/reclaim` | Verify and atomically install a compact physical SQLite rebuild | `RECLAIM ATTENTION DATABASE` |

Bodies reject unknown fields. The web UI uses the shared confirmation sheet,
then sends the exact server-owned confirmation. Maintenance requests use the
long-operation timeout and refresh the inventory on completion.

`make test-storage-governance-eval-harness` runs the provider-free evaluator
contract. `make test-storage-governance-live-eval` drives the compiled running
API against only `storage-live-eval/governance` and writes JSON/HTML evidence
under the coverage directory (also on failure); it is part of
`make test live_evals=true`.

## App database maintenance

The App database is per principal/workspace, at
`scopes/{principal}/{workspace}/apps/app_store.sqlite3` beneath the runtime root.
The Storage page lists it separately from package files and displays encryption,
database/WAL/SHM sizes, the scope-relative location and the last integrity result
from the current session. Health is not inferred from a successful file listing.
The production inventory lives in `magician-comms::channel_assist::governance`.
It shares the App maintenance policy and action descriptors with the lib-side
inventory, so the live route and fixture inventory expose the same owner actions.
SQLite shared-memory sidecars contribute to inventory file and byte totals;
reading these sizes does not open or authenticate a database connection.

`POST /api/magician/v2/storage/actions/app-store` accepts `operation`
(`verify`, `optimize`, `reclaim`) and its matching confirmation string. It uses
the same server-authenticated App scope and registry instance as ordinary App
operations; a conflicting workspace query is refused. A missing database stays
missing. This is an owner maintenance endpoint, not a capability granted to an
App or an ordinary App data-access bypass.

The registry first drains the scope through a fair asynchronous maintenance
gate, then acquires scoped write admission and drains its scoped connection
leases. Ordinary readers and writers wait for this gate before claiming a
blocking worker, so maintenance waiters cannot exhaust unrelated scopes’ workers.
Maintenance, writer and background-turn gates are shared by database path across
separately constructed registry services, as well as clones. This matches the
process-owned connection pool; an API owner and a background owner cannot enter
the same database through unrelated locks. Distinct runtime roots retain distinct
gates even when their principal/workspace names match. The gate directory is weak
and lazy: it neither initializes absent stores nor caps the number of Apps.
The SQLCipher opener validates the encryption, schema and
scope. Integrity checking verifies encrypted page authentication and SQLite
structure. Optimization checkpoints the WAL and refreshes planner statistics.
Reclamation uses transactional encrypted `VACUUM`, then checkpoints and checks
integrity again. No logical App records are deleted, no plaintext database is
created, and other scoped databases can continue. A busy external checkpoint
is reported; reclamation refuses to begin when that initial checkpoint is busy.
Reports include page/free-page counts, elapsed time and main/WAL/SHM sizes.
Maintenance callbacks must never recursively acquire a pooled connection.

Tests: `make test-app-registry-admission` (SQLCipher lifecycle and
maintenance fixtures), `make test-app-storage` (HTTP route and production
inventory owner), `make check-app-runtime`.

## Automatic Channel Assist and Feed maintenance

The service starts this worker after binding HTTP, then waits 15 seconds. It
checks metadata every 30 minutes for databases at least 64 MiB. A table with
128 or more row groups averaging under 2,048 physical rows, or a database that
has grown beyond three times its last compacted size, becomes eligible. The
normal cooldown is 24 hours; 512 or more row groups bypass it. These values
live in `database_maintenance` in `magician-config.yaml` (repo/package seed and
live runtime config). Budget and scheduler changes take effect on restart.

One database is processed at a time through its live store owner. Every writer
and cloned reader participates in a per-database admission gate. An idle gate
is preferred; busy work retries after a minute. After six hours of deferral the
worker stops admitting new operations and waits up to 30 seconds for existing
operations to drain. Requests can queue for up to 30 seconds, with at most 64
waiting operations; excess or timed-out requests receive a retryable error.
Other databases and HTTP health stay available. No offline window is needed.

Compaction checkpoints the source, makes a recoverable backup, copies into a
fresh file, compares table counts, schema, indexes, views and sequence state,
then atomically replaces and reopens the file. A durable marker makes an
interrupted publication restore its checkpointed source at next open. New
operations resume only after the marker is durably retired and read handles
are rebound. An unsuccessful recovery closes admission until restart rather
than accepting writes against an uncertain generation. Logical mail/feed
records are not removed. A valid Feed database is never quarantined for size;
it is eligible for online maintenance instead. Corruption recovery has its own
separate policy.
The preflight requires free space exceeding twice the source size plus 64 MiB;
a failed run keeps the source and retries.

Channel Assist uses one DuckDB worker thread and a 1,024 MiB buffer budget;
Feed uses one thread and 256 MiB. Maintenance temporarily allows 2,048 MiB per
DuckDB instance and restores the ordinary budget afterward. Temporary spill
files are limited to 2 GiB. These are database buffer limits, **not a process
RSS ceiling**; DuckDB metadata, Rust objects and executable pages also consume
memory. Mail imports commit at most 128 messages per transaction. Annotation
lookups query at most 128 unique thread IDs at once while preserving caller
order, duplicates, account and workspace scope. Inventory never opens its own
Channel/Feed connection; row estimates for these live stores are unavailable.

### Maintenance visibility

`GET /api/magician/v2/storage/maintenance` returns the authenticated scope's
latest Channel Assist and Feed status, without opening either database or
scanning the storage tree. Responses use `Cache-Control: private, no-store`.
States are `idle`, `running`, `completed`, `deferred`, `failed`, and `disabled`.
The small atomic status file beside each database records last completion,
reclaimed bytes and duration. A run from a previous process is shown as
interrupted/deferred until checked again. Failures have sanitized user messages
and detailed server logs. Setting `enabled: false` disables the worker while
leaving connection resource budgets in effect.

Automatic compactions also emit the existing scoped `ActivityStarted`,
`ActivityProgress` and `ActivityFinished` events (`kind=background`,
`workload_class=system`, `operation=database_compaction`). They appear in Runtime
Activity. Web Storage settings and iOS/Android Settings show persisted status
and last completion, polling every ten seconds while visible/active. Status
reads remain available while the database is being replaced. Manual compactions
retain their existing action reports and compaction ledger.

Tests: `make test-database-maintenance`.
