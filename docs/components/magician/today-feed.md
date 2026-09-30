# Today Feed API

`GET /api/magician/v2/today` is the backend projection for the unified UI
`/today` route. It combines attention, deliveries, memory changes, active work,
and follow-ups into the operator-facing Today sections.

## Response Modes

The endpoint has two response modes:

- **Preview mode**: callers omit `section`. The handler returns bounded previews
  for every section using `per_section` rows per section.
- **Section page mode**: callers send `section`, `limit`, and optional `cursor`.
  The handler returns rows only for the requested section while keeping counts
  and digest metadata available for the full projected set.

Supported `section` values are:

- `needs_you`
- `followups`
- `active_work`
- `delivered`
- `changed`

## Query Parameters

| Parameter | Mode | Behavior |
| --- | --- | --- |
| `per_section` | Preview | Clamped to `1..=20`; controls the all-section preview size when `section` is omitted. |
| `section` | Section page | Selects one Today section to return. |
| `limit` | Section page | Clamped to `1..=50`; defaults to the preview `per_section` value. |
| `cursor` | Section page | Opaque cursor from the previous response's `section_page.next_cursor`. |
| `digest_offset` | Digest page | Zero-based row offset for the What Changed digest only. |
| `today` | Any | The reader's own local date as `YYYY-MM-DD`. Every Today date predicate is evaluated against it. |

### `today=` — the reader's date, not the server's

The server cannot know the reader's timezone. `today=YYYY-MM-DD` is the date
the client is showing; the collector evaluates `due today`, `due tomorrow` and
`overdue` against it.

- **Absent** falls back to the UTC date: Today is polled by clients whose deploy
  we do not control, and a hard failure is worse than the imprecision. (The tasks
  list date lanes, a different surface, reject a missing `today=`.)
- **Malformed** is rejected with `400 today_must_be_yyyy_mm_dd`; silent fallback
  would hide the defect.
- Resolved **once per request** from the clock reading that stamps
  `generated_at`; nothing downstream re-reads the clock for a date.

Section page mode applies paging after the backend has projected, deduped,
sorted, and filtered visibility state. Route pages stay stable with the same
semantics as the preview response.

The collector caps the projected source set at 1,050 rows before final section
paging in both modes. Preview mode still returns only `per_section` rows per
tab; counts come from the same bounded collection window as section pages.

## One corpus walk per projection

`FeedApi::today_projection` reads the task corpus once: the Follow-ups walk is
hoisted and every other consumer reads off it; `valid_task_ids` is
`task_source`'s own ids.

- Internal ids: `FeedApi::live_internal_task_ids` reads
  `ListIndex::scope_entries` for `ListKind::Internal`, filtering on the indexed
  `status` column. It falls back to the walk exactly when
  `TaskApiV3::indexed_task_page` does (no index, `is_ready()` false, query error).
- Monitor lane: `ArtifactV2Service::list_scope_monitor_updates_from` takes the
  caller's listing and reads `monitor_revision` off the row. Invariant:
  `monitor_revision == 0` iff `monitor_spec.is_none()`; `update_task` is the only
  writer of `monitor_spec` and bumps the revision in the same manifest write;
  nothing clears a spec.
- `/feed` and `/feed/counts` read `ListIndex::scope_entries` for `ListKind::Task`
  through the same `valid_task_ids`.

One `/today` does exactly three corpus walks: the hoisted Follow-ups walk plus
two inside `ArtifactV2Service::list_attention_items` (user root, internal root).
`ArtifactV2Service::task_corpus_walks()` counts them; a test asserts the total.

## Listing reads take no cross-process lock

`V3ReadApi::get_task` takes the writer's exclusive `flock` because it may
replay an abandoned multi-write journal (`TaskWriteReconciler::recover_task_writes`).
`ArtifactV2Service::read_task_record_for_listing` drops the lock and replay and
keeps:

- `validate_task_id` (no path out of scope),
- the deletion marker (keeps mid-delete tasks out),
- `validate_task_scope_containment` (no symlinked cross-scope record),
- `read_task_record_unlocked` identity checks (`task_id`, `principal`,
  `workspace` match the directory).

Safe because every workspace write is `write_bytes_atomic` (old or new inode); a
partial commit is the same staleness a serial listing had; writers
(`persist_task_record`, the reducer's `with_task_write_lock`) recover under the
guard before writing, and `commit_multi_write_journal_path` refuses to start
while a journal is pending. An incomplete record is a skipped row. `get_task`
and all mutating callers still lock and replay.

## Blocking I/O on the `/today` path

Synchronous file operations run in `spawn_blocking`:

| what | where it went |
| --- | --- |
| `memory/users/knowledge.json` read/parse | `today_meeting_followup_items_from_memory_off_reactor` — only the read moves; the projection borrows the corpus listing and stays on the reactor |
| Today digest cache (with `fsync` when the fingerprint moved) | read awaited in `spawn_blocking`; write **detached** (response does not depend on it) |
| `today_visibility_state.json` | `today_visibility_state_off_reactor` |
| `pending_diff_approvals` (walk of `code_change_proposals/`) | `ArtifactV2Service::pending_diff_approvals_off_reactor`; a join failure runs inline rather than reporting "nothing pending" |

The digest fingerprint hashes item titles and summaries, so it moves every poll
during a run. `attention_dismissed_state.json` is read only by
`POST /feed/attention/dismiss`, not by `today_projection`.

## Projection cache

`magician/src/magician_v2/today_projection_cache.rs` caches the five
`Vec<TodayItem>` lanes — **not the response** — so one computation serves
preview and every section page (paging varies per request; lanes do not).

- **Key**: `(principal, workspace, projection date)`. The date is mandatory:
  readers in different timezones must not share, and membership changes at
  midnight with no write.
- **Scope half = the scope's directory**, not the request spelling.
  `scope_dir_segments` maps `:` `/` `\` `*` `?` `"` `<` `>` `|` to `_` and folds
  empty/`.`/`..` to `default` (`user:1` ≡ `user_1`). `TodayCacheKey::new` and
  `invalidate_scope` both normalise; `TodayCacheKey` fields are private.
- **TTL**: 10 s (clients poll at 20–30 s; covers a view's preview + section
  pages).
- Lanes are cached **after** visibility filtering (dismiss/snooze are per-scope,
  and scope is in the key).
- **Best effort**: miss means compute; cache failure never fails a request; a
  failed corpus read returns 500 and is not stored.

`today_projection_cached` fronts `FeedApi::today_projection`. Computation runs
outside the map (read guard dropped before `await`, insert after), so no
`DashMap` guard crosses an `await`.

- The response reports the projection's own `generated_at`.
- `freshness.source` is `live_projection` or `cached_projection`.
- Attention route events are appended once per computed projection, not per poll.

### Invalidation on write is load-bearing, not hygiene

Clients refetch their page after removing a row; a stale hit would resurrect it.
Every Today-affecting write makes the scope's entries unservable:

| write | route |
| --- | --- |
| Today visibility (`dismiss`, `snooze`, `mark_seen`, `restore`) | `POST /today/items/{item_id}/visibility` |
| Attention dismissal and undismissal | `POST /feed/attention/dismiss`, `/undismiss` |
| A Today item action that creates or promotes a task | `POST /today/items/{item_id}/actions/{action_id}` |
| Deleting one feed row | `DELETE /feed/items/{item_id}` |
| Clearing the feed (or one thread's rows) | `DELETE /feed/items` |
| Purging orphaned rows | `POST /feed/purge-orphans` |
| Every learning action that removes its row — confirm, edit-and-confirm, archive a candidate, archive an insight, save one to memory, turn one into a follow-up task | `POST /feed/learnings/{candidate_id}/…`, `POST /feed/insights/{insight_id}/…` |
| Every task record write — create, edit, due-date change, status change, complete, archive | anything reaching `TaskWriteReconciler::commit_task_writes` |
| Every task delete | `ArtifactV2Service::remove_task_dir_for_delete`, through `TaskWriteReconciler::task_record_removed` |

- Learning actions invalidate via `remove_feed_item_with_event`; a removal that
  found nothing invalidates nothing.
- Task writes bypass `FeedApi` (Follow-ups is projected from `list_tasks`). The
  cache is owned by `ArtifactV2Service` and borrowed by `FeedApi`.
  `TaskWriteReconciler::commit_task_writes` commits and invalidates in one call:
  a write set touching `manifest.json`, `state/task_state.json` or
  `task_refs.json` invalidates; execution-only commits do not.
  `commit_multi_write_journal_path` and `recover_multi_write_journal_path` are
  `pub(in crate::magician_v2::artifact_v2)` with one caller each, enforced by
  `only_the_reconciler_reaches_a_task_write_journal`. Journal replay also
  invalidates (tests `a_recovered_journal_reaches_the_list_index_and_todays_cache`,
  `a_replay_under_a_reducer_op_drops_todays_projection_with_no_index_present`).
- Most reducer transitions (including `reduce_runtime_signal`,
  `reduce_step_event`) carry the task record, so a scope with a running task
  drops the projection per tick. Unlike the list index, this cache does **not**
  skip invalidation when no indexed column moved: lanes are built from full
  `TaskListItemV3`s (`last_progress_at` alone can change a lane).
- **Invalidate after the write commits**, never before (a concurrent read could
  repopulate pre-write state).
- **Drop the scope, not one date** (entries either side of UTC midnight are both
  wrong).

#### Stamp on write, not drop on write

`invalidate_scope` bumps a per-scope `AtomicU64` (keyed by the same normalised
scope directory). An entry records the counter value when its computation
*started*; `get` serves it only while the counter is unchanged.

- Write path cost: one shard read and one `fetch_add`.
- A walk that straddles a write is not stored: `today_projection_cached` reads
  `TodayProjectionCache::write_stamp` before the walk and stores via
  `insert_if_unwritten_since`, which declines if the counter moved. The
  requester still gets the projection.
- Out-stamped entries are reclaimed by the TTL-only sweep on next insert
  (comparing stamps inside `DashMap::retain` would invert lock order).

#### What the staleness guarantee actually covers

Every write in the table is visible to its author on the next request. Up to
one TTL late:

1. Writes reaching `FeedStore` without `FeedApi` — V3 projection adapter, feed
   materializer, task and agent-learning projections, monitors API.
2. A snooze expiring (`today_apply_visibility_state` compares `snoozed_until`
   with the projection's `generated_at`; no write to invalidate on).
3. A `TaskRecord` written outside the reconciler — none today, but
   `ArtifactV2Workspace::write_json_atomic_path` is still reachable.

#### Not implemented: single flight

`get_or_insert_with` holds nothing between check, compute and insert, so two
concurrent misses on a cold key both walk the corpus.

### Section order and cursors

The sections do not share an ordering; the cursor is `{updated_at}:{item_id}`:

| section | order | tiebreak |
| --- | --- | --- |
| `needs_you` | `updated_at DESC`, stable sort over concatenated sources | none |
| `delivered` | `list_items`' `ORDER BY updated_at DESC, id DESC`, never re-sorted | descending |
| `active_work` | the same, never re-sorted | descending |
| `changed` | `updated_at DESC`, then ascending id | ascending |
| `followups` | `priority DESC`, then `updated_at ASC` | not `updated_at` order |

`followups` always takes the linear scan: a cursor whose row is dismissed
resumes by a key the lane is not sorted on. `/channel-assist/follow-ups`
encodes `{priority}:{updated_at}:{item_id}`; Today sections carry the two-part
key. Design: `docs/archive/plans/2026-07-30-list-pagination-and-index-design.md` §9.

## UI Contract

The unified UI maps canonical tab URLs to cursor-backed section requests:

- `/today?tab=followups` -> `section=followups&limit=8`
- `/today?tab=followups&page=2` -> `section=followups&limit=8&cursor=<stored page cursor>`

The store merges a section response into the existing Today state instead of
replacing all sections. Counts still come from the backend response, while row
arrays are updated only for the active section in section page mode.

Needs You and Followups are separate backend sections. Needs You represents
owner approval/intervention items; Followups represents channel-assist
obligations and promise/comms follow-up cards.

Active Work is sourced from running feed rows but is additionally checked
against current task metadata when present. Rows whose task metadata says the
task is `pending`, terminal (`completed`, `failed`, `cancelled` / `canceled`),
or has no active root execution are withheld from Active Work so stale feed
events do not make finished or not-yet-started tasks look live.

Task progress materialization also clamps late progress events against the
current task status. A stale action-progress event must not turn a task whose
record is already `pending`, `completed`, `failed`, or `cancelled` back into a
`running` feed row.

## Source Actions and Meeting Follow-ups

Today source actions use a source-agnostic adapter registry keyed by
`source_kind`. An adapter advertises generic `FeedAction` descriptors, declares
how an item links to durable tasks, and translates an action into an execution
plan. The API executes plans through one route:

`POST /api/magician/v2/today/items/{item_id}/actions/{action_id}`

`meeting_action` is an adapter. A meeting action that has no linked task
advertises **Create task**. Creation uses a deterministic task id, so retrying
the POST cannot create a duplicate. The projection also recognizes an explicit
`task_id`, the durable meeting-action provenance marker in a task description,
or an exact action-title match within the same meeting thread. It does not use
cross-thread fuzzy title matching.

The task description retains the action text, meeting title/date, thread id,
meeting route, and stable source id. A successful response returns the canonical
task and a task navigation target. Clients refresh Today and open that task.
Once linked, the original meeting-action row is suppressed for every task
status. Therefore completing, failing, or cancelling the task removes it from
Follow-ups without causing the original meeting action to reappear.

Meeting rows keep `summary` bounded for list rendering, while metadata carries
the complete `detail_markdown` and `meeting_summary`. Native clients therefore
show the full action and marked-up meeting context even when the original
meeting thread has expired. A visible meeting row is authoritative evidence
that no live task was reconciled; clients must open its native detail instead of
following a possibly stale meeting URL or `linked_task_id` hint.

## Web Message Follow-ups

The web Today Follow-ups tab also loads actionable email and message annotations
from `/api/magician/v2/channel-assist/follow-ups`. This is an asynchronous,
cursor-paginated lane rendered below ordinary Today follow-ups. Its visibility,
count, current range, and page count are reactive to the loaded channel result;
an initially empty render must not leave later-arriving message cards hidden.
