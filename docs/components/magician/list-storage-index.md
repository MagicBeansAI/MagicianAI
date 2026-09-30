# The List Storage Index

`magician_v2::storage::list_index` is the SQLite cache that task, internal-task
and monitor lists seek into, so page 50 costs the same as page 1. Attention's
`/today` sections do not, and structurally cannot; see
[Today cannot move onto this index](#today-cannot-move-onto-this-index).
`/feed/attention` lanes already seek in the feed store. Design history:
`docs/archive/plans/2026-07-30-list-pagination-and-index-design.md`
§5-§10.

## Files stay the source of truth

The index is a **rebuildable cache and never a second source of truth**. Delete
`list_index.db`, restart, and every list must be identical. Nothing may live in
the index that cannot be recovered by walking the disk.

## Design

- **SQLite via `rusqlite`**: six lane predicates plus counts plus a sort plus a
  keyset seek.
- **Scope is a column, not a database per scope.** Every list filters by
  principal/workspace. A scope is spelled the way
  `ArtifactV2Workspace::scope_dir_segments` normalises it; a raw `ScopeRef`
  matches no rows.
- One table, `list_entries`, keyed `(kind, id)` where `kind` is
  `task | internal | monitor | attention`.
- `agent_id` has a covering `(kind, principal, workspace, status, agent_id)`
  index so background admission (Town Square) can ask for distinct agents with
  active tasks without building cards; the file walk is the fallback while the
  index is not ready.
- **Two keysets, because the surfaces have two orders.** Every list sorts
  `updated_at` descending; they differ on same-millisecond ties:

  | index | order | who pages in it |
  | --- | --- | --- |
  | `list_keyset` | `updated_at DESC, id DESC` | tasks, internal tasks, attention's cursor |
  | `list_keyset_id_asc` | `updated_at DESC, id ASC` | `/monitors` |

  `ListTieBreak` names the pair once; `ListPageQuery::tie_break` picks one and
  the same value builds the `ORDER BY`, the cursor `WHERE` and the Rust
  predicate the fallback walk uses. Monitors stay ascending because changing it
  would reorder rows under live cursors (the three-platform monitors fixture
  pins it).
- `paused` is a column because `/monitors?state=active|paused` is a filter. It
  is written by the handler's own `schedule_is_paused(parsed_schedule(…))`, and
  is `false` with no schedule or an unparseable one. (A schedule object without
  `kind`, e.g. `{"paused": true}`, fails `TaskSchedule` parsing, so `/monitors`
  calls it ACTIVE.)
- Every comparison relies on SQLite's default BINARY collation, matching Rust
  `&str` ordering. No column may be declared `COLLATE NOCASE`.

## Lifecycle

The schema version lives in `PRAGMA user_version` and is **3**. Every column is
recoverable by walking the records.

| on open | what happens |
| --- | --- |
| file missing | empty schema created; a rebuild is owed |
| version matches, schema probes clean | reused as-is |
| version differs **in either direction** | file discarded, empty schema created |
| unreadable, or readable without our schema | file discarded, empty schema created |

The only reuse condition is exact version equality: adopting an older file
would fail queries on missing columns and silently degrade that surface to the
walk forever.

A damaged index is **discarded, never propagated**: `open` does not return an
error for a corrupt cache; the caller learns what happened from
`ListIndex::opened()`. Discarding removes the `-wal` and `-shm` sidecars too.

## Populating

`ListIndex::rebuild_from_disk(scopes_root)` walks
`scopes/<principal>/<workspace>/{tasks,internal_tasks}/<task_id>/` once,
reading each record's `manifest.json` and `state/task_state.json`.

- **A record whose files vanish mid-walk is skipped**, counted in
  `RebuildReport`.
- **A unit root that cannot be read skips its unit.** `NotFound` means no tasks
  of that kind; anything else is unknown content (see
  [A missing root is not an unreadable one](#a-missing-root-is-not-an-unreadable-one)).
  A skipped unit counts in `RebuildReport::units_failed`, and a rebuild that
  skipped anything **does not mark itself complete**: readiness stays
  `in_progress`, readers keep the walk, and reconciler watermarks stay. The
  whole walk is not aborted, because readiness is stamped *before* the walk and
  aborting would leave the index unready forever.
- **Resumable, per notmuch.** Progress is recorded per `(root, scope)` unit in
  the same transaction as that unit's rows. An interrupted rebuild resumes at
  the next unindexed unit; a crash mid-unit re-walks it whole. When the last
  unit lands, progress rows and `reconcile:%` watermarks are cleared and the
  index is marked ready.

`ListIndex::index_task_from_disk(scopes_root, scope, task_id)` is the write
path hook. It **re-reads the record from disk** rather than trusting the
caller's copy, so the index reflects what a rebuild would reproduce. A record
gone from both roots has its rows removed (the delete path). It **writes
nothing when the indexed columns are identical**, compared inside the write
transaction; `ReindexOutcome::rewritten` reports whether it wrote.

### What the index must hide

- `kind = task` applies the user-visible gate (`lifecycle != internal`, id not
  `system:*`, agent and author not `__system__`), and skips ids that also exist
  under `internal_tasks/` (`ArtifactV2Workspace::task_dir` probes the internal
  root first).
- `kind = monitor` is a user-visible task with a `monitor_spec` whose status is
  not `archived` — a *second* row for the same record, since a monitor lists on
  both surfaces.
- **`kind = attention` is unused**: nothing writes or reads it. The variant
  reserves a stable `kind` string.

## The queries

| call | replaces |
| --- | --- |
| `page(ListPageQuery)` | `skip(offset).take(limit)` over a fully-loaded `Vec` |
| `total(kind, scope, lane, today)` | `tasks.len()` after materialising everything |
| `lane_counts(kind, scope, today)` | a pass over the whole pool per badge |
| `monitor_page(MonitorPageQuery)` | loading, sorting and scanning every task for the cursor's row |
| `scope_entries(kind, scope)` | a full corpus walk kept only for its ids |

### `scope_entries` — membership, not a page

Returns **every** row a scope holds for one kind, unpaged and unordered, for
`/today`, `/feed` and `/feed/counts` "does this task still exist" filters. It
is deliberately not `page` with a huge limit (a `usize::MAX` limit casts to
`-1`, which is correct only by accident). Rows carry `status` and `lifecycle`
because `FeedApi::live_internal_task_ids` drops terminal internal tasks; only
membership, not freshness, is claimed.

### Paging

`page` is a **keyset seek** when a cursor is given and an `OFFSET` otherwise; a
cursor supersedes an offset. The cursor's wire form is
`{updated_at}:{url-encoded id}`, byte-for-byte `encode_attention_lane_cursor`.

**The SQL `OFFSET` and the reported offset differ.** A seek needs no SQL offset,
but the envelope reports `COUNT(*)` of rows at or before the cursor via
`ListTieBreak::precedes_clause` (the complement of `follows_clause`); reporting
`0` would show page one as current while rendering page two.

`total` is the lane's total over the whole scoped pool, never the page. Counts
run over the whole pool before any lane filter; every lane is present even at
zero. `today` is the READER's local date, passed in explicitly.

### One definition of each lane

Lane predicates are those in `magician/src/magician_v2/task_lanes.rs`:

- `lane_clause` matches **exhaustively** on `TaskLane` and returns SQL plus
  binds, so a new lane cannot compile without SQL.
- `every_lane_agrees_with_task_lanes_over_one_fixture` asserts identical ids per
  lane between `TaskLane::matches` and the SQL.

SQL details that are not noise:

- `due_date <> ''` in `overdue` — an empty string sorts before every date.
- `substr(due_date, 1, length(?)) = ?` in `today` (a `starts_with`), so a full
  timestamp still counts as due today.

## What a reader sees during a rebuild

`ListIndex::is_ready()` is false from rebuild start until it finishes; the
marker is persisted, so a crash mid-rebuild still reads unfinished. While
false, handlers serve from the file walk — a half-built index looks exactly
like a complete one with fewer tasks.

### The gate that decides to rebuild is `rebuild_is_owed`, not the open outcome

`ListIndexOpen::needs_rebuild()` is **false for `Reused`**, and an interrupted
rebuild leaves a current-version file whose marker reads `in_progress`. The
boot gate is therefore `ListIndex::rebuild_is_owed()`
(`needs_rebuild() || !is_ready()`), asked by `magician-bin/src/main.rs` through
`list_index_rebuild_owed`, which treats an unreadable index as owing a rebuild
rather than failing the boot.

## `magician --reindex` — the recovery route

Discards `list_index.db` (with sidecars) and walks every scoped task root in
the foreground, then exits — no server, workers or scheduler. The discard is
the point: a boot rebuild resumes and trusts committed units, so it cannot fix
rows that are *wrong*. `ListIndex::open_discarding` with
`DiscardReason::OperatorRequested` keeps it out of the logs as corruption. It
prints units walked, entries indexed/skipped/hidden, unparsed timestamps, and
the readiness marker.

## Who opens it, who writes it, who reads it

| seam | where |
| --- | --- |
| opened at boot | `magician-bin/src/main.rs`, beside the attention funnel store; rebuilt off the reactor when `list_index_rebuild_owed` says so |
| handed to the service | `ArtifactV2Service::set_list_index`, a `OnceLock` mirroring `set_attention_funnel_store` |
| reconciled on write | `TaskWriteReconciler::commit_task_writes` — the ONE call every task-record journal commit goes through — and `remove_task_dir_for_delete` for deletes |
| reconciled on replay | `TaskWriteReconciler::recover_task_writes` — the ONE call every journal *replay* goes through, on all sixteen paths that take a task write lock |
| reconciled periodically | `magician-bin/src/main.rs`, a five-minute ticker (see [Reconciling](#reconciling--the-pass-that-bounds-the-drift)) |
| read (tasks) | `magician-api/src/task_api_v3.rs` `list_tasks` and `list_internal_tasks`, through `TaskApiV3::indexed_task_page` |
| read (monitors) | `magician-api/src/monitors_api.rs` `list_monitors_v3_handler`, through `indexed_monitor_page` |

Both write hooks are **best effort** and run *after* the journal commits: a
write that landed on disk is never reported failed because the cache could not
be updated; the periodic pass repairs it. The delete hook runs whatever
happened: `remove_task_dir_for_delete` reports what it removed and cleans up
before returning any error.

### Cost on a running execution

Eleven of the reducer's sixteen transitions carry the task record; two fire per
tick during a run:

| transition | fires on | carries |
| --- | --- | --- |
| `reduce_runtime_signal` | every runtime signal | `task_record_writes` — manifest, state, refs |
| `reduce_step_event` | every `StepCompleted` / `StepFailed` | `task_state_and_refs_writes` |
| `reduce_execution_nonterminal` | every non-terminal outcome | `task_state_and_refs_writes` |

Each pays a `spawn_blocking` hop and two small JSON reads **inside
`with_task_write_lock`**, whose `write_lock` is per-*reducer* (process-wide),
on top of the per-task cross-process flock. The unchanged-columns check makes
step events (which only advance unindexed `last_progress_at`) write nothing; a
runtime signal moves `updated_at` (indexed, and the sort key) and owes the
write. The reconcile stays inside the reducer mutex so two writers cannot
interleave `reindex_task`'s disk read with the other's commit. Today's
invalidation is not skipped alongside — its projection is built from full
`TaskListItemV3`s (see `today-feed.md`).

### Write and replay chokepoint

`TaskWriteReconciler` commits the journal **and** reconciles as one operation;
all three writers commit through it. A commit carrying `manifest.json`,
`state/task_state.json` or `task_refs.json` reindexes; an execution-only commit
does not. The reducer holds the same `Arc` as the service.

A commit is not the only way a record reaches disk:
`recover_multi_write_journal_path` replays a persisted journal on **sixteen**
paths (every reducer op via `with_task_write_lock`, `get_task`,
`get_execution`, every plan transition, `update_task`, `persist_task_record`),
completing a commit that died between "journal durable" and "all writes
applied". `recover_task_writes` replays *and* reconciles, decides from the
landed paths whether a task record was touched, and reconciles on a failed
replay too.

- Both raw primitives (`commit_multi_write_journal_path`,
  `recover_multi_write_journal_path`) have exactly one caller inside
  `artifact_v2` and are `pub(in crate::magician_v2::artifact_v2)`;
  `only_the_reconciler_reaches_a_task_write_journal` enforces it by reading the
  sources.
- Only `commit_task_writes`, `recover_task_writes`, `task_record_removed` and
  accessors are public; the reindex/deindex/invalidate halves are private.
- Not covered: `write_json_atomic_path` pointed at `task_state.json` would
  change an indexed `status` unheard (nothing does this). Failed reindexes log
  under `[LIST-INDEX]` and the periodic pass repairs them.

### Which surfaces actually read this

| surface | reads the index? | |
| --- | --- | --- |
| Tasks | **yes** | `task_api_v3::list_tasks` via `indexed_task_page` |
| Internal tasks | **yes** | `task_api_v3::list_internal_tasks`, same path |
| Monitors | **yes** | `monitors_api::list_monitors_v3_handler` via `indexed_monitor_page` |
| Attention — `/feed/attention` | never needed to | already a keyset seek in `FeedStore` |
| Attention — `/today` | ordering only, and cannot do more | `row_follows_cursor`; no rows, no query |

Monitors use the index with an unchanged contract. `ListIndex::monitor_page` is
its own query because **a monitor cursor is a bare `task_id`**. The cursor is
looked up **under the page's filter**: a cursor naming a since-paused monitor
is stale for `state=active` and ends pagination with an empty page. `offset` is
`COUNT(*)` of rows at or before the cursor. Page rows are **built from records
on disk** (one `get_task` per id, bounded by `limit`); the index supplies
ordering and identity only. Rows whose record was deleted, archived or lost its
spec are skipped, and `next_cursor` is minted from the INDEX's last id.

### Covering the read path

Only `magician-bin/src/main.rs` fills the `OnceLock` in production.
`magician_v2::test_support` wires `wire_test_list_index` (ready),
`wire_unready_test_list_index`, or neither, before any record exists. Tests
must drive the service's own `reducer()` — a separately built
`FilesystemArtifactV2Reducer` has no index and not the shared Today cache. The
discriminator: `ListIndex::remove` drops one row while its record stays on
disk, so a `total` one lower proves the index served. Read-path tests live in
`task_api_v3::task_list_index_read_path_tests` and `monitors_api`, with both
due-date shapes in the corpus.

## Reconciling — the pass that bounds the drift

A task record can land on disk without the index being told.
`ListIndex::reconcile_from_disk` walks the disk and repairs disagreement, so a
writer nobody hooked costs **one interval of drift instead of an unbounded
wrong answer** — regardless of which door the drift came through.

### What one pass does

Per `(kind, scope)` unit (`UnitKind::Task` / `Internal`, the same units the
rebuild walks, both iterating `UnitKind::ALL`):

- one `readdir` of the unit root — the id set the unit is authoritative over;
- `stat` each id's two record files and re-read **only** records that moved
  since the unit's watermark;
- remove rows the index holds under ids the `readdir` did **not** name (the
  tasks unit sweep covers `monitor` rows too).

```rust
pub struct ReconcileReport {
    pub units: usize,         // (kind, scope) units that finished
    pub units_failed: usize,  // units that errored and kept their watermark
    pub records_read: usize,  // task records re-read from disk
    pub repaired: usize,      // records whose indexed columns actually moved
    pub removed: usize,       // rows dropped, per row
}
```

**`records_read` is the design invariant made observable:** an unchanged corpus
must cost stats, not reads (`an_idle_pass_reads_no_records`). Repair is
`index_task_from_disk`. **The connection lock is never held across the
filesystem walk.** **A half-built index is not reconciled at all** — the pass
returns empty when `is_ready()` is false, or it would advance watermarks past
records the rebuild has yet to reach.

### The watermark

One per unit, in `list_index_meta` under
`reconcile:{kind}:{principal}:{workspace}`.

- `SystemTime::now()` is sampled **before** the `readdir`, inside
  `reconcile_unit`, so a write landing mid-pass is caught next pass.
- Absent reads as the epoch (first pass reads everything once); unparseable
  reads as the epoch and **warns** (otherwise a silent full walk every pass).
- It advances **only on success**; one bad unit does not abort the others.
- **A completed rebuild deletes every `reconcile:%` key**, or the next pass
  would skip exactly the records the rebuild fixed.

Why a watermark and not an `indexed_at` column: no schema bump, no per-row
state, and the comparison is file mtime against stored file mtime (same clock);
`indexed_at` would compare mtime against logical `updated_at`, and skew would
degrade to a full walk.

### The cadence, and why repair is silent

Five minutes, on a `tokio::time::interval_at` ticker spawned after the boot
rebuild is decided; the first tick is one interval out so boot is not
disk-bound. Missed ticks delay rather than queue. Each pass runs on
`spawn_blocking`; failures and panics log at `warn` and the loop continues. It
logs at `info` only when `repaired > 0 || removed > 0` — **the repair count is
the only signal that a writer hook has gone missing**.

### A missing root is not an unreadable one

**A missing unit root is not an error** (a scope with no internal tasks has no
`internal_tasks/`). **An unreadable one is.** `NotFound` is the only error
interpreted; EACCES, EMFILE, ENOTDIR and others fail the unit, log the cause
with `{:#}`, and leave its watermark unmoved. Why: the scan's id set drives the
orphan sweep, so a root read as empty would delete every row in the scope and
advance past them. An entry that cannot be stat'd fails the unit rather than
being dropped from the id set.

`walk_task_root` draws the same line for rebuilds: `commit_unit` clears a
unit's rows before inserting and records progress, so an empty read would
**mark the scope done**. The unit is skipped and the rebuild refuses to
complete (see [Populating](#populating)).

### What the mtime gate actually stats, and why it is not the directory

The gate stats the two files `read_task_entry` reads — `manifest.json` and
`state/task_state.json` (named by `manifest_path` and `task_state_path`). A
third input added to `read_task_entry` without being added here would be
invisible to the reconciler.

**Files, not directories:** `write_bytes_atomic` renames within the file's
immediate parent, so writing `state/task_state.json` moves `<task>/state/`, not
`<task>/`, and `status`/`updated_at` come only from that file.

Deletion is the one thing an mtime cannot report, so the `readdir` runs every
pass and drives orphan removal.

- An **absent** record file is a state change: with no row, candidate and
  current are both empty and `index_task_from_disk` declines to write.
- An **unreadable** record file is not: it fails the unit rather than deleting
  rows or advancing the watermark past an unchecked record.

The comparison is `>=`, so a file written in the watermark's own tick costs one
redundant read instead of being skipped forever
(`the_gate_re_reads_a_file_written_in_the_watermarks_own_tick`, anchored on the
newer of the two files).

### What the pass does not repair

- **A record that stats cleanly but will not open.** `read_json` collapses
  missing, unreadable and malformed into "no rows", so such a record has its
  rows deleted as though gone — deliberately the same tolerance every reader
  has.
- **The `monitor` and `attention` kinds get no unit of their own.** `monitor`
  rows are produced and swept by the tasks walk. `attention` has no producer
  (see [What the index must hide](#what-the-index-must-hide)); a stray
  `attention` row is the one kind no sweep removes.
- **The Today projection cache** — TTL-bounded at 10 seconds; see
  `today-feed.md`.
- **A unit that keeps failing** holds its watermark and stays stale until a
  rebuild; `units_failed` says so.

## When the handlers decline the index

The index answers the exact question asked, or not at all. Handlers fall back
to the walk when:

- no index was wired in;
- `is_ready()` is false (the whole of a rebuild, including across a crash);
- the request filters on something the index does not carry: `status=`,
  `query=`, `agent_id=`, `ui_thread_id=`;
- the request asks for an order other than `updated_at` descending.

Declining is **per request, not a latch**. The fallback walk sorts with the
index's tiebreak whenever the order is the keyset order (`updated_at` alone
leaves ties in `read_dir` order, fatal for a cursor). `row_follows_cursor` is
the keyset predicate in Rust, beside the SQL it mirrors.

## Attention pages the same keyset

`attention_lane_facade`'s `cursor_offset` is a `partition_point` over
`row_follows_cursor` — the index's predicate applied to a cursor whose wire form
is already byte-for-byte `ListCursor::encode`. The seek is **checked, not
trusted**: `list_attention_lane` preserves the CALLER's ordering, which callers
do not agree on, so when the row before the seek IS the cursor's row the answer
costs `log n`; otherwise the linear scan runs.

### Which attention surface actually scans

| surface | how a page is taken |
| --- | --- |
| `/feed/attention` — the five lanes (requests, approvals, escalations, failed, running) | **already a store seek.** `list_feed_attention_lane` → `FeedStore::list_attention_lane_page` puts the cursor in the SQL `WHERE` and fetches `limit + 1` rows. |
| `/today` — the five sections | **the scan.** `list_attention_lane` takes a `&[T]` the caller has already built whole, and `page_from_offset` slices it. |

`/feed/attention` is seekable because **its lane is stored**
(`feed_items.attention_lane` on insert; `feed_attention_items` rows carry
`lane`).

### Today cannot move onto this index

The Today **projection** cannot. Only its membership question ("is this task
still there?") moved: `FeedApi::valid_task_ids` and
`FeedApi::live_internal_task_ids` read `scope_entries` and fall back to the walk
when the index declines.

**No write produces a Today item.** `TodayItem` and `TodaySectionId`
(`magician-api/src/feed_api.rs`) are persisted nowhere; section and priority are
computed per request from a join across DuckDB `feed_items`, the V3 attention
projection, the monitor update ledger, on-disk knowledge and thinking maps,
`list_tasks` for live task existence, and `Utc::now()` for the dismissed/snoozed
gate (`today_apply_visibility_state`). The only Today-keyed file,
`today_visibility_state.json`, holds per-`item_id` overrides, not membership.
Indexing Today would mean reproducing that join inside `list_entries` — a
second source of truth. Attention takes only ORDERING from the index.
`TodayItem.id` is `today:<section>:<feed row id>`, so Today and
`/feed/attention` cursors are not interchangeable.

## The envelope

`{ tasks, pagination: { total, limit, offset, has_more, next_cursor } }`, plus
`counts` when the reader supplied their local date. `next_cursor` is always
present and `null` on the last page, so clients never confuse "no more pages"
with "no cursor support".

**The legacy unpaginated `{tasks}` body is unchanged.** It is entered only when
`limit`, `offset` and `cursor` are all absent, and is answered before any of the
paginated path's work.
