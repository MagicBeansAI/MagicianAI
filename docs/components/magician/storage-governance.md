# Storage Governance

The canonical owner list, class, tier and readiness ledger is
[`storage-catalog.yaml`](storage-catalog.yaml), documented in
[Storage Abstraction](storage-abstraction.md). `/storage` (also in Settings and
the command palette) is its scope-aware operator projection (`governance_id`);
drift fails `make check-storage-catalog`. Per store it reports apparent and
allocated size, WAL size, file count, approximate DuckDB rows, partition range,
retention, safety class, inventory completeness and permitted actions. Each
store has its own safe cleanup rule (e.g. thread tombstones preserve identity;
the LLM recovery journal outlives its derived Parquet facts).

Track B activation evidence is `GET /api/magician/v2/storage/activation` and
`magician storage status` (readiness-qualified); the panel shows remaining
blocks and has no cutover action. Gate 3 drills live in
`magician/src/magician_v2/gate3/`; budgets are remote-durable except lines named
`Local`. `make test-storage-budgets` enforces wall-clock budgets on an idle
machine (`make test-rust` only records them); gate closure is a compile-time
assertion in `magician-storage/src/gate3.rs`. See
[adr-2026-09-01-performance-capacity](../magician-storage/adr-2026-09-01-performance-capacity.md).
Switching the desktop Runtime provider (`local_file` / `silverbullet_space`)
does not move data, and reaching `remote_ready` does not delete the local
source. New implicit-path bypasses fail `make check-typed-storage-boundaries`.

## Ownership and lifecycle

| Store | Class | Automated lifecycle | Operator action |
| --- | --- | --- | --- |
| `analytics/analytics.duckdb` | Regenerable catalog | Store-owned checkpoints | Verified copy-compaction |
| `channel_assist/channel_assist.duckdb` | Lifecycle-managed comms state | Provider reconciliation only; no age deletion | Verified copy-compaction only |
| `ui_threads/ui_threads.duckdb` | Authoritative thread index | Checkpoint at most once per 30 s | Copy-compaction preserving soft-delete tombstones |
| `feed/feed.duckdb` | Regenerable projection | Source-owned refresh | Purge only rows proven orphaned against the task list |
| `social/social.db` (`social_sqlite`) | Lifecycle-managed SQLite | Scope-isolated; copy-compaction via VACUUM | Compact safely |
| `attention_funnel.db` | Lifecycle-managed SQLite | 30 days and per-scope cap | Inspect only |
| `resurfacing.db` | Lifecycle-managed SQLite | 90 days and per-scope caps | Inspect only |
| `attention_learning.db` | Shared file, scope-owned learning ledger | 30–3650 day scoped retention; active evidence and referenced decisions protected | Optimize / preview and clean scoped history / verified rebuild |
| `analytics/browser_engine_usage.sqlite3` | Scope-owned observability SQLite | Sanitized command attempts; 90 days, 50,000 rows per scope | Inspect via Observe → Stats |
| `apps/app_store.sqlite3` | Authoritative encrypted App registry, records, grants, receipts | Created lazily by its authenticated owner; migrations, retention, purge are App lifecycle | Integrity check, checkpoint/optimize, encrypted compaction |
| `apps/packages` | Immutable package bytes | Pinned staging, create-only atomic promotion, bounded stale-staging recovery, exact bytes revalidated before registry publication; import adds bytes only after bounded authenticated archive admission; retention retires bytes | Inspect only |
| `apps/attachments` | Dormant restricted attachments | Future retention/purge owner only | Inspect only |
| `apps/exports` | Dormant restricted export archives | Future archive/encryption/purge owners only | Inspect only |
| `apps/captures` | Dormant restricted prompt/debug/provider captures | Future data-policy owner only | Inspect only |
| `apps/evaluations` | Dormant restricted evaluation artifacts | Future data-policy/purge owner only | Inspect only |
| `events`, `memory_events`, legacy `llm_calls`, `llm_embeddings`, `llm_dispatch` | Observability Parquet | Completed-day compaction; 90-day retention | Compact completed days / apply retention |
| `analytics/activity_rows` | Activity spine, one row per completed span | `dt=…/hour=…`; hours fold after 2 h, days after 48 h; **7-day** detail, then rolled up | Compact; Apply retention runs the tiered sweep (7 days is not expressible as `retention_days`) |
| `analytics/activity_rollups` | Per-hour summaries of expired spine detail | Keyed `(dt, hour, kind, workload_class, agent_id, outcome)`; **13-month** retention | Expired by the tiered sweep |
| `events.jsonl` | Per-scope append-only transport log | Compactor keeps whichever is larger: the last 24 h or the newest 2,000 events per scope | Inspect only |
| Canonical LLM calls, attempts, tool calls, capture gaps | Governed observability facts | Rolling active-day and closed-day compaction; 90-day retention | Compact / apply retention; immutable revisions are the corruption fallback |
| Restricted LLM content/context, tombstones, access audit | Restricted | Configured restricted-content/fact retention | Inspect policy and size |
| LLM trace and restricted-content journals | Authoritative recovery state | Journal-owned committed-segment lifecycle | No generic destructive action |
| `storage_governance/compaction_metrics.json` | Content-free observability | Atomic ledger, ≤256 events per scope | Inspect / clear metrics only |

The inventory is read-only: loading `/storage` never creates `apps/`, opens
SQLite, stages packages or enables an app route. Only authenticated registry
writes, package stage/recovery or import create owned paths (import never
creates the registry; export refuses to create an absent tree), sharing an exact canonical
`apps/scope-binding.json` so scopes that normalize to the same names cannot
share state. Missing paths show as zero bytes; SQLite `-wal` companions count;
symlinked components are rejected and child symlinks skipped. Walks are bounded (apps 20,000 entries / 64 levels; others
50,000 / 64); hitting a ceiling marks the entry and aggregate **partial**. App
entries expose no generic maintenance action. Snapshots share scope-keyed gates
with maintenance plus a two-second per-scope cache (≤256 scopes, invalidated by
any maintenance mutation), so concurrent refreshes coalesce.

### What the inventory measures, and where

- **Paths follow the writer.** The transport log's sanitizer accepts only
  `[A-Za-z0-9_-]` up to 128 characters and routes the rest to
  `_quarantine/_quarantine`; `ArtifactV2Workspace` accepts any non-traversal
  segment. Where they disagree (e.g. `user.name`) the inventory shows the
  base-relative directory that holds the bytes. The sanitizer warns once per
  scope; past 64 distinct scopes (`QUARANTINE_REPORT_CAP`) it logs one "further
  quarantine routing will not be reported" line. Each key half is capped at 128 bytes (`is_safe_scope_component`).
- **WAL sidecars** are looked up per engine: SQLite `<file>-wal`,
  DuckDB `<file>.wal`, journals none (they stage via temp and rename).
- **`retention_days`** is the policy's real window (transport log's rolling
  day, funnel 30, resurfacing 90) or `None`. The transport log's value is
  `EVENTS_RETENTION_WINDOW_MS` rounded **up** with a one-day floor, never `0`.

## Attention-learning maintenance

Three separate operations:

- **Optimize now**: `ANALYZE` on the serving-hot decision, impression and
  canonical-membership tables, then `PRAGMA optimize`. Online; keeps every row; refreshes planner
  statistics (a small statistics write); does not reclaim disk.
- **Clean old history**: a read-only preview for the selected scope and window;
  apply needs a second confirmation and re-evaluates the same rules. Active
  evidence, current models, idempotency anchors and referenced decisions or
  projections stay protected. Removes rows; no smaller-file promise.
- **Reclaim disk space**: keeps all rows. Under the writer and an exclusive
  cross-process lease, builds a `VACUUM INTO` sibling in
  `auto_vacuum = INCREMENTAL` mode, verifies integrity, foreign keys, schema,
  file identity pragmas, AUTOINCREMENT state, incremental mode and exact row
  counts, fsyncs. Reads stay available during the build; only the final WAL
  checkpoint and atomic swap enter a bounded fail-fast read drain. A hard-link
  rollback copy is kept until the writer and read pool reopen. Global (one shared
  file); a second process or concurrent reclaimer gets a bounded conflict.

**Automatic scoped retention**: the size guard is never why the store cannot
shrink. Above `AUTO_SCOPED_RETENTION_MAX_DB_BYTES` (8 GiB) a pass deletes only the first
`AUTO_SCOPED_RETENTION_OVERSIZED_SLICE_MS` (24 h) past the scope's oldest
retention anchor, every `AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS` (60 s); a
healthy store runs daily.

- The gate records a pass's finish and refuses overlap; it takes the fast
  interval as its floor before stat-ing the file, and a store found healthy
  restores its previous timestamp. Over the guard each consecutive empty pass
  doubles the cadence (capped at daily), and a productive pass is followed by
  `AUTO_SCOPED_RETENTION_WRITER_SHARE_FACTOR` (4) times its `writer_hold_ms`
  (writer time summed over its transactions, not wall clock) of quiet, so
  maintenance takes ≤ a fifth of the writer.
- Eligibility is surveyed on a read connection; the writer is held only for
  the small-row delete. Projections then retire
  `RETENTION_PROJECTION_BATCH_ROWS` (4) per transaction (~1,000 member rows
  each) under the `DELETE_PROJECTION` guards; the legacy-migration quarantine
  follows in its own statement.
- Anchors move: a terminal rank-recompute job retires with its outcome's time
  or its own, whichever is inside the window. Outcomes retire before decision
  items; per-table report counts use the delete predicates.
- Body sweeps run only after a pass that retired rows. Feature vectors are
  swept inside the delete transaction (via
  `attention_history_item_feature_idx` and
  `attention_history_binding_feature_idx`, the latter also serving the FK
  check). Canonical item and diagnostic revisions are released with their
  projections (**Bodies** below).
- After any batch that deleted rows, `PRAGMA incremental_vacuum` runs in
  512-page chunks for ~200 ms (a tenth of `REQUEST_PATH_WRITER_WAIT`). It frees
  pages only in `auto_vacuum = INCREMENTAL` (new stores; older files switch at
  next reclaim); in `NONE` mode **Reclaim disk space** does.
- A completed pass logs `retired_rows`, `empty_passes`, `elapsed_ms`.

**Triggers.** Each projection compute schedules the keep-latest prune
(`CANONICAL_PROJECTION_HISTORY_KEEP`, 64) and the gated retention pass. So idle
stores still shrink, `AttentionLearningService::spawn_storage_maintenance`
(started after HTTP readiness; supervised, a panic restarts it after 60 s;
first tick after 2 min plus up to 60 s jitter, then every 15 min) schedules
both for every scope holding a projection, decision or delivery, 5 s apart;
the gate decides whether retention is due.

**Bodies.** Retiring a projection (prune, scoped retention, `delete_scope`)
probes only the item/rank/decision digests its members held. The full walk
(`sweep_orphan_canonical_bodies`) runs only at the start of **Reclaim disk
space**.

**Retention policy.**

- Served decisions, deliveries and their items keep
  `delivery_retention_days` (30; deliveries compact 30 days after expiry); a
  decision is not retired (keep-latest
  `DECISION_UNREFERENCED`, scoped `RETIRABLE_DECISION_SQL`) while any delivery
  from it survives. Bandit attribution, `attribution_ref` resolution and
  `reconstruct_served_decision_id` read decision items, never the projection.
- The canonical projection (~3 MB page set) stops being pinned by an expired
  delivery that nothing attributes to (no impression, outcome, rank-recompute
  job or bandit update names its decision or pages) after
  `attention_learning.bandit.expired_delivery_retention_hours` (default 24) in
  `DELETE_PROJECTION`. Its own decision no longer pins it; outcomes, jobs,
  impressions and live or attributed deliveries do. A late rank recompute falls
  back to the decision-digest lookup and may find nothing.
- Scoped retention never releases a projection before its own cutoff (its
  delivery bound is the earlier of the two windows).

**Compact write formats** (old rows stay readable; no boot rewrite; migration
is `ADD COLUMN` plus one `DROP INDEX`):

- Decision diagnostics move `decision_id`, `served_rank` / `baseline_rank` /
  `learned_rank` into
  `attention_canonical_projection_members.decision_{served,baseline,learned}_rank`;
  the stripped body is content-addressed (`diagnostic_digest("decision", body)`).
  Projection identity and API responses are unchanged; other body shapes keep
  the legacy full form.
- Decision items: `feature_contracts_json` is `''` on new rows and
  `feature_contracts_id` references `attention_feature_contract_sets`
  (`decision_item_feature_contracts_json` reads both). `item_json` is kept.
- Delivery items byte-identical to the canonical decision item
  `(projection_id, candidate_id)` store `attribution_item_json` NULL and
  `attribution_ref = 1`; a differing copy is stored as before.
- `attention_canonical_projection_members_page_idx` duplicated the PRIMARY KEY
  autoindex and is dropped.

Serving never counts history per request: a transactional, trigger-maintained
scope counter (backfilled once at migration) tracks verified impressions, and
hydration is keyed by the authorized decision id.

**Background storage owner.** Full sweep at startup and every six hours over
real scoped directories; a five-minute pass rolls active canonical LLM
partitions once their tail reaches 64 files, and the full sweep folds any tail
of an ended day, however small. One scope's failure is isolated. Embedding telemetry shares this owner, so pruning cannot race
compaction; restricted content keeps its own worker. Shutdown cancels and joins the
owner after response producers quiesce and before the canonical and
compatibility LLM pipelines publish final batches.

## Governed reads must consume compaction output

Canonical LLM fact revisions (`part-call_fact-<hash>-rN.parquet`) are kept on
purpose as a corruption fallback and never pruned, so compaction only helps if
readers use it. **Select sources through
`llm_fact_compactor::governed_dataset_sources*`, never by listing a partition.**
They return `[compacted] + uncompacted_tail` only after validating manifest
scope, schema, source-set checksum, existence of every compacted source, the
compacted file's length and BLAKE3 checksum, and `source_row_count ==
compacted_row_count`; any mismatch falls back to the full raw set (lossless,
fail-closed). The legacy batch equivalent is
`parquet_maintenance::partition_sources`; `llm_calls` attempts use
`analytics_api::governed_attempts_read_source_sql`. Reading raw revisions is
correct, only slower.

Validation is cached in process: a partition's read set while the directory,
manifest and compacted object keep length and mtime, and (today and yesterday
only) the manifest's verdict while manifest and compacted object are
unchanged. A hit still checks every covered source exists; any change
re-validates from disk.

## DuckDB copy-compaction

DuckDB `VACUUM` does not shrink the file, so the three live DuckDB owners copy
and swap:

1. Hold the writer mutex (and cross-process lease where present); checkpoint.
2. Fingerprint table DDL, exact row counts, and index, view and sequence state.
3. `COPY FROM DATABASE` into a sibling file; fsync.
4. Require an exact fingerprint match on the copy.
5. Keep the source as a rollback backup, publish, run owner bootstrap/migrations,
   verify again.
6. Delete the backup only once the published connection works and the parent
   directory is durable.

A post-publication failure quarantines the new file and restores the original.
Symlinks and special files are refused. Mail compaction is physical only: it
never deletes messages, threads, annotations, feedback, reconciliation cursors,
writing preferences or audit events; mail deletion stays provider- and
user-owned.

## Parquet generation protocol

Completed UTC partitions (never today's) with at least two raw batches compact
into a generation-named replacement:

```text
memory_events.compacted.01K....parquet
_compact/memory_events.compaction-manifest.json
```

The prior generation stays authoritative while the next is written and
verified. The manifest atomically selects a new generation only after row-count,
checksum, file-length and fsync guards; raw inputs are re-fingerprinted and,
with inactive generations, removed only after the manifest is durable. A changed source, symlink, row
mismatch, malformed output or lock timeout fails the partition closed. Late
batches stay visible until merged. With a malformed or missing manifest,
readers recover only a sole valid generation (multiple candidates or checksum
drift fail closed); the next guarded sweep republishes the manifest.

The 90-day lifecycle covers runtime events, memory events, local embedding
batches, legacy/canonical LLM calls, provider attempts, tool-call lineage,
capture gaps and dispatch telemetry; the cutoff-date partition is kept. It does
not touch mail, tasks, memory facts, SQLite lifecycle stores, restricted-content
stores or the recovery journal. Sweep steps are independent: a failure is
logged against its partition or dataset and the sweep continues.

### The activity spine's two-level fold

`analytics/activity_rows` partitions `dt=YYYY-MM-DD/hour=HH/` and folds a closed
hour after **two hours** (five-minute lane) and a closed day after **48 hours**
(then removing its `hour=` directories), both via the generation/manifest
protocol, so a late row in a folded hour merges on the next sweep.

Retention is **tiered**: detail is kept 7 days, summarised into
`analytics/activity_rollups` (count, p50, p95, total duration per
`(dt, hour, kind, workload_class, agent_id, outcome)`), then removed; rollups
are kept 13 months. `count` and `sum_duration_ms` are additive; `p50_ms`/`p95_ms`
are not, so wider-window percentiles need detail.

**The marker is the commit record.** `_rollup/rolled-up.json`, written in the
**rollup** partition before detail is deleted, lets a crashed pass finish the
delete instead of re-aggregating. It records which sources each rollup
generation covers, so a late batch in a rolled-up day becomes an additional
generation; the fold skips days that already have one. No double count: the marker is removed only with the whole rollup
partition, object names hash their source fingerprints (a re-run overwrites),
and unnamed objects leave the `.parquet` namespace before publishing. Quarantine
runs only when publishing or superseding a generation, never on the
delete-without-aggregate paths. Readers select the marker's generations and list the directory only when no
valid marker exists; the writer refuses on an unreadable marker rather than
reading it as empty.

**Rolling up is what authorises the delete.** The roll-up keeps the detail when:

- the day or any `hour=` directory holds a Parquet object `partition_sources`
  cannot account for (it recognises `batch_*` files and the manifest's own
  generation; verified-but-unpruned `batch_*` files are excluded);
- only a legacy detail-partition `_rollup/rolled-up.json` vouches for the day
  (remove it to force one re-roll; a day holding only that marker directory is
  still deleted);
- the rollup marker is malformed or has a newer `schema_version`;
- any other summarise-and-verify error.

A day stuck refusing escalates from `warn` to `error` once overdue by more than
the retention window, and never gives up on its own.

### Rolling canonical LLM generations

Canonical LLM compaction never deletes raw revisions: a versioned manifest
selects a checksum-verified generation covering an explicit subset, and later
appends are read as a raw tail. Every five minutes (and at startup), once the
active UTC partition's tail reaches 64 files, the prior object and tail merge
into a new generation; the six-hour sweep finalizes closed partitions.
Version-1 exact-set manifests stay readable as safe prefixes. Readers check the
selected object's length and checksum; only a generation roll rechecks all
covered raw fingerprints. Missing, malformed, cross-scope, duplicate-key or
corrupt generations fail closed to raw revisions. Publication: temp Parquet,
row/idempotency/scope/sequence validation, fsync, atomic rename, atomic
manifest write; an append racing compaction joins the next tail.

## Compaction impact metrics

Every effective manual, startup, six-hour and five-minute compaction records a
content-free event in the active scope (owning DuckDB or dataset, partitions, files and rows folded, bytes
before/after/reclaimed, duration, trigger; canonical LLM adds
`query_files_avoided`). Newly written bytes never count as reclaimed. The
ledger is one atomic JSON file of ≤256 events (snapshot shows 24); no-op sweeps
add nothing; a malformed or unsupported ledger reports unhealthy; a write failure only
warns and never fails a verified compaction. `/storage` shows reclaimed space,
compacted files, query files avoided, per-area totals, recent runs and the
ledger's own footprint. **Clear metrics** (confirmed) deletes only this ledger,
never databases, Parquet, manifests, journals or other scope data.

## HTTP contract

All routes require a workspace-bound bearer; the server resolves principal and
workspace from it.

| Method and route | Purpose | Required body confirmation |
| --- | --- | --- |
| `GET /api/magician/v2/storage` | Typed inventory and permitted actions | n/a |
| `POST /api/magician/v2/storage/actions/compact-databases` | Compact selected `analytics`, `channel_assist`, `ui_threads` owners | `COMPACT DATABASE` |
| `POST /api/magician/v2/storage/actions/compact-parquet` | Compact completed generic and canonical LLM partitions | `COMPACT PARQUET` |
| `POST /api/magician/v2/storage/actions/apply-retention` | Scope-bound 30–3650 day window plus the spine's 7-day/13-month sweep | `APPLY RETENTION` |
| `POST /api/magician/v2/storage/actions/clear-compaction-metrics` | Clear only this scope's metrics ledger | `CLEAR COMPACTION METRICS` |
| `POST /api/magician/v2/storage/actions/attention-learning/optimize` | Refresh planner statistics online | `OPTIMIZE ATTENTION` |
| `POST /api/magician/v2/storage/actions/attention-learning/retention-preview` | Preview scoped cleanup; `retention_days` 30–3650 | none; confirmation is rejected |
| `POST /api/magician/v2/storage/actions/attention-learning/retention-apply` | Apply the previewed scoped rules | `CLEAN ATTENTION HISTORY` |
| `POST /api/magician/v2/storage/actions/attention-learning/reclaim` | Verify and atomically install a compact SQLite rebuild | `RECLAIM ATTENTION DATABASE` |

Bodies reject unknown fields. The web UI sends the exact server-owned
confirmation after its confirmation sheet; maintenance uses the long-operation
timeout and refreshes the inventory on completion.

`make test-storage-governance-eval-harness` runs the provider-free evaluator
contract. `make test-storage-governance-live-eval` drives the running API
against `storage-live-eval/governance` only, writing JSON/HTML evidence under
the coverage directory (also on failure); it is part of
`make test live_evals=true`.

## App database maintenance

The per-scope App database is
`scopes/{principal}/{workspace}/apps/app_store.sqlite3` under the runtime root.
Storage lists it apart from package files with encryption, database/WAL/SHM
sizes, scope-relative location and this session's last integrity result (health is not inferred from a file
listing). The
production inventory (`magician-comms::channel_assist::governance`) shares the
App maintenance policy and action descriptors with the lib-side inventory.
SHM sidecars count toward totals; reading sizes never opens or authenticates a
connection.

`POST /api/magician/v2/storage/actions/app-store` takes `operation` (`verify`,
`optimize`, `reclaim`) and its confirmation string, using the same
server-authenticated scope and registry as ordinary App operations. A
conflicting workspace query is refused; a missing database stays missing. It is
an owner endpoint, not an App capability or data-access bypass.

Maintenance drains the scope through a fair async gate, then scoped write
admission and connection leases; ordinary work waits on the gate before taking a
blocking worker. Maintenance, writer and background-turn gates are keyed by database path
(distinct per runtime root) and shared across clones and separately built
registry services (matching the process-owned connection pool); they are weak,
lazy, and never create stores or cap App count.

The SQLCipher opener validates encryption, schema and scope. Integrity check
verifies SQLCipher page authentication and SQLite structure;
optimize checkpoints the WAL and refreshes statistics; reclaim runs encrypted
`VACUUM`, then checkpoints and rechecks integrity. No App record is deleted, no
plaintext database is created, other scopes keep working. A busy checkpoint is
reported, and reclaim refuses to start on one. Reports include page/free-page
counts, elapsed time and main/WAL/SHM sizes. Maintenance callbacks must never
recursively acquire a pooled connection.

Tests: `make test-app-registry-admission`, `make test-app-storage`,
`make check-app-runtime`.

## Automatic Channel Assist and Feed maintenance

Settings live in `database_maintenance` in `magician-config.yaml` (repo seed and
live config; changes apply on restart):

| Rule | Value |
| --- | --- |
| Start | 15 s after HTTP binds; check every 30 minutes |
| Size floor | databases of at least 64 MiB |
| Eligible | a table with ≥128 row groups averaging <2,048 rows, or growth beyond 3× last compacted size |
| Cooldown | 24 hours; ≥512 row groups bypass it |
| Deferral | an idle gate is preferred; busy work retries after a minute; after six hours, admission closes and existing work gets up to 30 s to drain |
| Request queue | up to 30 s, at most 64 waiting; excess or timed-out requests get a retryable error |
| Free-space preflight | above twice the source size plus 64 MiB |

One database is processed at a time through its live store owner, behind a
per-database admission gate covering every writer and cloned reader; other
databases and HTTP health stay available; no offline window. Compaction
checkpoints, keeps a recoverable backup, copies into a fresh file, compares
table counts, schema, indexes, views and sequence state, then atomically
replaces and reopens (as in [DuckDB copy-compaction](#duckdb-copy-compaction)). A durable
marker restores the source after an interrupted publication, operations resume
only after it is retired and read handles rebound, and a failed recovery closes
admission until restart. Logical mail/feed records are never removed, and a
valid Feed database is never quarantined for size (corruption recovery is a
separate policy). A failed run keeps the
source and retries.

Channel Assist runs one DuckDB thread with a 1,024 MiB buffer; Feed one thread
with 256 MiB. Maintenance temporarily allows 2,048 MiB per instance, then restores the
budget; spill is
capped at 2 GiB. These are buffer limits, **not a process RSS ceiling**. Mail
imports commit ≤128 messages per transaction; annotation lookups query ≤128
unique thread IDs at once, preserving order, duplicates and scope. Inventory
never opens Channel/Feed connections, so their row estimates are unavailable.

### Maintenance visibility

`GET /api/magician/v2/storage/maintenance` returns the scope's latest Channel
Assist and Feed status without opening either database or scanning storage
(`Cache-Control: private, no-store`). States: `idle`, `running`, `completed`,
`deferred`, `failed`, `disabled`. An atomic status file beside each database
records last completion, reclaimed bytes and duration; a previous process's run
shows interrupted/deferred. Failures carry sanitized messages (details in server
logs). `enabled: false` disables the worker but keeps connection budgets.

Automatic runs emit scoped `ActivityStarted`, `ActivityProgress` and
`ActivityFinished` events (`kind=background`, `workload_class=system`,
`operation=database_compaction`) shown in Runtime Activity. Web Storage
settings and iOS/Android Settings poll status every ten seconds while visible;
status reads work during replacement. Manual compactions keep their action
reports and ledger.

Tests: `make test-database-maintenance`.
