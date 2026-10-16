# The List Storage Index

`magician_v2::storage::list_index` is the SQLite cache that task, internal-task
and monitor lists seek into, so page 50 costs the same as page 1. Attention's
`/today` sections do not and cannot use it; see
[Today cannot move onto this index](#today-cannot-move-onto-this-index).
`/feed/attention` lanes already seek in the feed store. Design history:
`docs/archive/plans/2026-07-30-list-pagination-and-index-design.md`
§5-§10.

## Files stay the source of truth

The index is a **rebuildable cache, never a second source of truth**. Delete
`list_index.db`, restart, and every list must be identical. Nothing may live in
the index that cannot be recovered by walking the disk.

## Design

- **SQLite via `rusqlite`**: one table `list_entries` keyed `(kind, id)`, `kind`
  in `task | internal | monitor | attention`.
- **Scope is a column**, spelled as `ArtifactV2Workspace::scope_dir_segments`
  normalises it; a raw `ScopeRef` matches no rows.
- A covering `(kind, principal, workspace, status, agent_id)` index serves
  background admission (Town Square: distinct agents with active tasks, no
  cards built); the file walk is its fallback while the index is unready.
- **Two keysets**, differing on same-millisecond ties:

  | index | order | who pages in it |
  | --- | --- | --- |
  | `list_keyset` | `updated_at DESC, id DESC` | tasks, internal tasks, attention's cursor |
  | `list_keyset_id_asc` | `updated_at DESC, id ASC` | `/monitors` |

  `ListTieBreak` names the pair; `ListPageQuery::tie_break` drives the
  `ORDER BY`, the cursor `WHERE` and the fallback walk's predicate. Monitors stay
  ascending so live cursors keep their order (three-platform monitors fixture).
- `paused` backs `/monitors?state=active|paused`, from the handler's
  `schedule_is_paused(parsed_schedule(…))`; `false` with no or an unparseable
  schedule (`{"paused": true}` without `kind` fails `TaskSchedule`, so ACTIVE).
- Default BINARY collation matches Rust `&str` ordering; no column may be
  `COLLATE NOCASE`.

## Lifecycle

Schema version: `PRAGMA user_version` **5** (4: canonical app runs are internal;
5: soft-archived records leave the active task, internal-task and monitor
catalogues, so retained archived directories cannot inflate totals). Older
caches rebuild.

| on open | what happens |
| --- | --- |
| file missing | empty schema created; a rebuild is owed |
| version matches, schema probes clean | reused as-is |
| version differs **in either direction** | file discarded, empty schema created |
| unreadable, or readable without our schema | file discarded, empty schema created |

Only an exact version match is reused (an older file would fail queries on
missing columns and degrade that surface to the walk forever). A damaged index is **discarded, never
propagated**: `open` returns no error, the caller reads `ListIndex::opened()`,
and the `-wal`/`-shm` sidecars go too.

## Populating

`ListIndex::rebuild_from_disk(scopes_root)` walks
`scopes/<principal>/<workspace>/{tasks,internal_tasks}/<task_id>/` once, reading
`manifest.json` and `state/task_state.json`.

- A record vanishing mid-walk is skipped and counted in `RebuildReport`.
- A unit root returning `NotFound` means no tasks of that kind; any other error
  skips its unit (see
  [A missing root is not an unreadable one](#a-missing-root-is-not-an-unreadable-one))
  and counts in `RebuildReport::units_failed`; the rebuild then **does not mark
  itself complete** (readiness stays `in_progress`, readers walk, watermarks
  stay) but is not aborted either (readiness is stamped before the walk).
- **Resumable, per notmuch**: progress per `(root, scope)` unit commits with its
  rows; a restart resumes at the next unit, and a crash mid-unit re-walks it
whole. Completion clears progress rows and
  `reconcile:%` watermarks and marks the index ready.

`ListIndex::index_task_from_disk(scopes_root, scope, task_id)` is the write-path
hook. It **re-reads the record from disk**, so the index matches a rebuild; a
record gone from both roots loses its rows. It **writes nothing when the indexed
columns are unchanged** (checked in the write transaction);
`ReindexOutcome::rewritten` reports whether it wrote.

### What the index must hide

- `kind = task` applies the user-visible gate (`lifecycle != internal`, id not
  `system:*`, agent and author not `__system__`) and skips ids also under
  `internal_tasks/` (`ArtifactV2Workspace::task_dir` probes that first).
- `kind = monitor` is a second row for a user-visible task with a
  `monitor_spec`, status not `archived`.
- **`kind = attention` is unused**; the variant only reserves the string.

## The queries

| call | replaces |
| --- | --- |
| `page(ListPageQuery)` | `skip(offset).take(limit)` over a fully-loaded `Vec` |
| `total(kind, scope, lane, today)` | `tasks.len()` after materialising everything |
| `lane_counts(kind, scope, today)` | a pass over the whole pool per badge |
| `monitor_page(MonitorPageQuery)` | loading, sorting and scanning every task for the cursor's row |
| `scope_entries(kind, scope)` | a full corpus walk kept only for its ids |

- **`scope_entries`**: every row of one kind in a scope, unpaged and unordered,
  for the
  `/today`, `/feed` and `/feed/counts` existence filters (`page` with
  `usize::MAX` would cast to `-1`). Rows carry `status` and `lifecycle` so
  `FeedApi::live_internal_task_ids` can drop terminal internal tasks. Membership only, not freshness.
- **`page`**: keyset seek with a cursor (which wins), `OFFSET` otherwise. Cursor
  `{updated_at}:{url-encoded id}`, byte-for-byte `encode_attention_lane_cursor`.
  Reported offset is `COUNT(*)` at or before the cursor
  (`ListTieBreak::precedes_clause`, complement of `follows_clause`), not `0`,
  which would show page one while rendering page two.
- **`total`** is the lane's total over the whole scoped pool, never the page;
  counts run over the whole pool before any lane filter, every lane present at
  zero. `today` is the reader's local date, passed in explicitly.
- **Lanes** come from `magician/src/magician_v2/task_lanes.rs`; `lane_clause`
  matches exhaustively on `TaskLane` (SQL plus binds, so a new lane cannot
  compile without SQL), and
  `every_lane_agrees_with_task_lanes_over_one_fixture` checks it against
  `TaskLane::matches`. `overdue` needs `due_date <> ''` (empty sorts before every date); `today` uses
  `substr(due_date, 1, length(?)) = ?` (a `starts_with`) so timestamps count.

## What a reader sees during a rebuild

`ListIndex::is_ready()` is false for the whole rebuild, persisted across
crashes; handlers serve the file walk meanwhile (a half-built index looks like
a complete one with fewer tasks). Because `ListIndexOpen::needs_rebuild()` is
false for `Reused`, and an interrupted rebuild leaves a current-version file
whose marker reads `in_progress`, boot asks
`ListIndex::rebuild_is_owed()` (`needs_rebuild() || !is_ready()`) via
`list_index_rebuild_owed` in `magician-bin/src/main.rs`, which treats an
unreadable index as owing a rebuild rather than failing the boot.

## `magician --reindex` — the recovery route

Discards `list_index.db` and sidecars, rebuilds in the foreground and exits
(no server, workers or scheduler); unlike a resuming boot rebuild it fixes wrong
committed rows. Uses `ListIndex::open_discarding` with
`DiscardReason::OperatorRequested` (not logged as corruption) and prints units
walked, entries indexed/skipped/hidden, unparsed timestamps and the readiness
marker.

## Who opens it, who writes it, who reads it

| seam | where |
| --- | --- |
| opened at boot | `magician-bin/src/main.rs`, beside the attention funnel store; rebuilt off the reactor when `list_index_rebuild_owed` |
| handed to the service | `ArtifactV2Service::set_list_index` (`OnceLock`, like `set_attention_funnel_store`) |
| reconciled on write | `TaskWriteReconciler::commit_task_writes`, the one call every task-record journal commit goes through; `remove_task_dir_for_delete` for deletes |
| reconciled on replay | `TaskWriteReconciler::recover_task_writes`, the one call every journal replay goes through (all sixteen task-write-lock paths) |
| reconciled periodically | `magician-bin/src/main.rs`, five-minute ticker ([Reconciling](#reconciling--the-pass-that-bounds-the-drift)) |
| read (tasks) | `magician-api/src/task_api_v3.rs` `list_tasks`, `list_internal_tasks` via `TaskApiV3::indexed_task_page` |
| read (monitors) | `magician-api/src/monitors_api.rs` `list_monitors_v3_handler` via `indexed_monitor_page` |

Write hooks are **best effort** after the journal commits: a landed write is
never reported failed; the periodic pass repairs misses. On delete,
`remove_task_dir_for_delete` drops the rows (`task_record_removed`) once
neither root holds the task, even if the delete also returns an error; if one
root survives, the rows stay so a retry can finish.

### Cost on a running execution

Eleven of the reducer's sixteen transitions carry the task record; these fire
during a run:

| transition | fires on | carries |
| --- | --- | --- |
| `reduce_runtime_signal` | every runtime signal | `task_record_writes` — manifest, state, refs |
| `reduce_step_event` | every `StepCompleted` / `StepFailed` | `task_state_and_refs_writes` |
| `reduce_execution_nonterminal` | every non-terminal outcome | `task_state_and_refs_writes` |

Each pays a `spawn_blocking` hop and two JSON reads inside
`with_task_write_lock` (whose `write_lock` is per-reducer, process-wide, on top
of the per-task cross-process flock), under the reducer mutex so
`reindex_task`'s read cannot interleave with another commit. Step events
(unindexed `last_progress_at`) write nothing; a runtime signal moves
`updated_at` (indexed, the sort key) and writes. Today invalidation is not
skipped alongside: its projection uses full `TaskListItemV3`s (`today-feed.md`).

### Write and replay chokepoint

`TaskWriteReconciler` commits the journal **and** reconciles for all three
writers (the reducer shares the service's `Arc`). Commits touching
`manifest.json`, `state/task_state.json` or `task_refs.json` reindex; an
execution-only commit does not.
`recover_task_writes` wraps `recover_multi_write_journal_path`, which finishes
durable-but-unapplied journals on **sixteen** paths (reducer ops via
`with_task_write_lock`, `get_task`, `get_execution`, plan transitions,
`update_task`, `persist_task_record`), and reconciles even on a failed replay.

- `commit_multi_write_journal_path` / `recover_multi_write_journal_path` each
  have one caller and are `pub(in crate::magician_v2::artifact_v2)`;
  `only_the_reconciler_reaches_a_task_write_journal` enforces it from the
  sources. Public: `commit_task_writes`, `recover_task_writes`,
  `task_record_removed`, accessors; the reindex/deindex/invalidate halves are
  private.
- Not covered: `write_json_atomic_path` on `task_state.json` would change an
  indexed `status` unheard (nothing does this). Failed reindexes log
  `[LIST-INDEX]`; the periodic pass repairs them.

### Which surfaces actually read this

| surface | reads the index? | |
| --- | --- | --- |
| Tasks | **yes** | `task_api_v3::list_tasks` via `indexed_task_page` |
| Internal tasks | **yes** | `task_api_v3::list_internal_tasks`, same path |
| Monitors | **yes** | `monitors_api::list_monitors_v3_handler` via `indexed_monitor_page` |
| Attention — `/feed/attention` | never needed to | already a keyset seek in `FeedStore` |
| Attention — `/today` | ordering only, and cannot do more | `row_follows_cursor`; no rows, no query |

Monitors keep their contract unchanged. `ListIndex::monitor_page` exists
because **a monitor cursor is a bare `task_id`**, looked up under the page's
filter (a since-paused monitor is stale for `state=active` and ends paging with
an empty page); `offset` is `COUNT(*)` at or before the cursor. Rows come from disk (one `get_task` per id, at most
`limit`); the index gives order and identity. Deleted, archived or spec-less
records are skipped; `next_cursor` is the index's last id.

### Covering the read path

Only `magician-bin/src/main.rs` fills the `OnceLock` in production. Tests wire
`magician_v2::test_support`'s `wire_test_list_index` (ready),
`wire_unready_test_list_index`, or neither, before any record exists, and drive
the service's `reducer()` (a separate `FilesystemArtifactV2Reducer` has no index
and not the shared Today cache). `ListIndex::remove` of one row with its record
on disk makes `total` one lower when the index served
(`task_api_v3::task_list_index_read_path_tests`, `monitors_api`; corpus has both
due-date shapes).

## Reconciling — the pass that bounds the drift

`ListIndex::reconcile_from_disk` repairs records written without telling the
index, so an unhooked writer costs **one interval of drift instead of an
unbounded wrong answer**. Per `(kind, scope)` unit (`UnitKind::Task` /
`Internal`, iterating `UnitKind::ALL`): one `readdir` (the authoritative id
set), a `stat` of each record's two files, a re-read (`index_task_from_disk`)
**only** of those newer than the watermark, and removal of rows for unlisted ids
(`monitor` rows included).

`ReconcileReport`: `units` (finished), `units_failed` (kept their watermark),
`records_read`, `repaired` (indexed columns moved), `removed` (per row). An idle
corpus costs stats, not reads
(`an_idle_pass_reads_no_records`). **The connection lock is never held across
the filesystem walk.** **A half-built index is not reconciled** (empty pass
while `is_ready()` is false, or watermarks would pass records the rebuild has
not reached).

### The watermark

One per unit in `list_index_meta`, key `reconcile:{kind}:{principal}:{workspace}`,
sampled from `SystemTime::now()` **before** the `readdir` in `reconcile_unit`
(a mid-pass write is caught next pass). Absent or unparseable reads as the epoch
(unparseable **warns**). It advances **only on success**, per unit; one bad unit
does not abort the others. **A completed rebuild deletes every `reconcile:%`
key**, or the next pass would skip the records it fixed. The watermark is a meta
row, not a per-row column: no schema bump, and mtime compares with mtime on one
clock.

### The cadence, and why repair is silent

Every five minutes (`tokio::time::interval_at`, spawned after the boot rebuild
is decided, first tick one interval out, missed ticks delay rather than queue),
each pass on `spawn_blocking`; failures and panics log at `warn` and the loop
continues. It logs at `info` only when `repaired > 0 || removed > 0`: **the repair
count is the only signal that a writer hook has gone missing**.

### A missing root is not an unreadable one

**A missing unit root is not an error** (no internal tasks means no
`internal_tasks/`); **an unreadable one is.** Only `NotFound` means empty; EACCES, EMFILE, ENOTDIR and the rest (or an entry that cannot be
stat'd) fail the unit, log with `{:#}` and keep its watermark, since an empty id
set would sweep every row in the scope and advance past them. `walk_task_root`
applies this to rebuilds (`commit_unit` clears then inserts and records
progress, so an empty read would mark the scope done), so the unit is skipped and the
rebuild stays incomplete (see [Populating](#populating)).

### What the mtime gate actually stats, and why it is not the directory

The files `read_task_entry` reads: `manifest.json` and `state/task_state.json`
(`manifest_path`, `task_state_path`); a new input there must be added here.
Files, because `write_bytes_atomic` renames within the parent, touching
`<task>/state/`, not `<task>/`. Deletion is the one thing mtime cannot report, so
the `readdir` runs every pass and drives orphan removal. An **absent** record
file is a state change (with no row, `index_task_from_disk` writes nothing); an
**unreadable** one fails the unit. The comparison is `>=`, anchored on the newer
of the two files, so a same-tick write costs one extra read
(`the_gate_re_reads_a_file_written_in_the_watermarks_own_tick`).

### What the pass does not repair

- **A record that stats cleanly but will not open**: `read_json` collapses
  missing, unreadable and malformed alike, so its rows are deleted (the same
  tolerance every reader has).
- **`monitor` and `attention` kinds have no unit**: `monitor` rows come from the
  tasks walk; a stray `attention` row (see
  [What the index must hide](#what-the-index-must-hide)) is never swept.
- **The Today projection cache** (10 s TTL; `today-feed.md`).
- **A persistently failing unit** (until a rebuild; `units_failed`).

## When the handlers decline the index

The index answers the exact question or not at all. Handlers walk, per request,
when no index is wired, `is_ready()` is false, the request filters on
`status=`, `query=`, `agent_id=` or `ui_thread_id=`, `archived` is not
`exclude`, or the order is not `updated_at` descending. The walk uses the index's tiebreak for keyset order
(not `read_dir` order); `row_follows_cursor` is the Rust keyset predicate.

## Attention pages the same keyset

`attention_lane_facade`'s `cursor_offset` is a `partition_point` over
`row_follows_cursor`, on a cursor already byte-for-byte `ListCursor::encode`.
`list_attention_lane` callers disagree on ordering, so the seek is verified:
`log n` when it lands on the cursor's row, else a linear scan.

### Which attention surface actually scans

| surface | how a page is taken |
| --- | --- |
| `/feed/attention` — the five lanes (requests, approvals, escalations, failed, running) | **already a store seek.** `list_feed_attention_lane` → `FeedStore::list_attention_lane_page`, cursor in SQL `WHERE`, `limit + 1` rows. |
| `/today` — the five sections | **the scan.** `list_attention_lane` over a caller-built `&[T]`, sliced by `page_from_offset`. |

`/feed/attention` seeks because its lane is stored (`feed_items.attention_lane`;
`feed_attention_items` rows carry `lane`).

### Today cannot move onto this index

Only membership moved: `FeedApi::valid_task_ids` and
`FeedApi::live_internal_task_ids` read `scope_entries`, walking when the index
declines. **No write produces a
Today item**: `TodayItem` and `TodaySectionId` (`magician-api/src/feed_api.rs`)
are never persisted, and are computed per request from DuckDB `feed_items`, the
V3 attention projection, the monitor update ledger, on-disk knowledge and
thinking maps, `list_tasks` (task existence), and `Utc::now()` for the
dismissed/snoozed gate (`today_apply_visibility_state`).
`today_visibility_state.json` holds per-`item_id` overrides only. Indexing Today
would reproduce that join in `list_entries`, a second source of truth; attention
takes only ordering from the index. `TodayItem.id` is
`today:<section>:<feed row id>`, so Today and `/feed/attention` cursors differ.

## The envelope

`{ tasks, pagination: { total, limit, offset, has_more, next_cursor } }`, plus
`counts` when the reader sent a local date; `next_cursor` is always present,
`null` on the last page (never confused with no cursor support). **The legacy
unpaginated `{tasks}` body is unchanged**, served when `limit`, `offset` and
`cursor` are all absent, before any paginated-path work.
