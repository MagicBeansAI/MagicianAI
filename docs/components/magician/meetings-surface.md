# Meetings surface — the console app over the meeting rails

The meetings console as an app: live capture state and transcripts, thread
history with keyword search, takeaways, upcoming calendar context, and the full
capture controls. **Additive alongside first-party `/observe`** — its meetings
sections, the TopBar capture dot, and the fast-poll lease are untouched. This
package is a new consumer of unchanged routes, never a route move. Rollback is
**disable**, never uninstall.

Design record:
`docs/archive/plans/2026-08-29-apps_platform_meetings-surface-increment.md`.
Rail-class re-open (drafted, separate workstream):
`docs/plans/2026-09-02-meeting-rail-class-reopen.md`.

## The four pieces

| Piece | Owner |
| --- | --- |
| `meetings_data` read binder (six bounded reads) | `magician/src/magician_v2/execution/meetings_data_provider.rs` |
| `magician.meeting-control` destination (five verbs) | `magician/src/magician_v2/meeting_control_contribution.rs` |
| The `meetings` system package + custom surface | `magician_data_v3/system/meetings/app/` |
| The shared capture-control audit | `magician/src/magician_v2/media_seam/meeting_control_audit.rs` |

No engine gets a second code base:

- The gws calendar read (account resolution, dedupe/merge, cache) lives in
  `magician/src/magician_v2/media_seam/meeting_calendar.rs`; `GET
  /meetings/upcoming` and the binder are both callers. The cache is keyed by the
  scope's capability auth root, so one scope's calendar is never served to
  another.
- The process chat store is published once at boot
  (`chat::storage::publish_global_chat_store`) so the binder's reads share the
  server's index. Read-only by contract; mutations go through the store's owner.

## `meetings_data` — the read binder

Pattern: the `evidence_data` / `thinking_maps_data` host-read binders. Closed
action set, fail-closed argument proofs, executor-owned runtime scope
(`__principal` / `__workspace` are injected; public values are assertions that
can never switch scope), fixed scan budgets, 8 KiB input and 512 KiB result
ceilings, 30-second timeout.

| Action | Shape |
| --- | --- |
| `active_session {}` | Both capture registries as one bounded page. |
| `list_threads {status?, text?, limit?, after_thread_id?}` | The scope's dated `meeting-` threads, newest first. |
| `read_thread {thread_id, session_id?, before_message_id?, limit?}` | One transcript page, backwards from newest. |
| `read_takeaways {thread_id?, limit?}` | The meeting rows of `user.research_findings`. |
| `upcoming_meetings {}` | The scope's cached next ~12 h of calendar meetings. |
| `search_meeting_memory {text, limit?}` | Bounded keyword retrieval across all three. |

**The registries are process-global; the reads around them are not.**
`active_session` projects every registered session on both rails — "is capture
running" must never be hidden from the operator (same as `GET /meetings/active`).
`session_id`, `mode`, `status`, `live`, `paused` and durations are registry-wide;
`thread_id`, `title` and `url` appear only when `in_scope` (the registry recorded
THIS scope as owner at spawn). A title or join link identifies a room, and the
first-party `latest_summary` is dropped entirely. A cross-scope row is visible
and useless, by intent.

Ownership is the recorded scope, not chat-index membership. Why: a meeting's
chat session is created lazily (the owner's capture would read as foreign), and
thread ids `meeting-<slug(title)>-<date>` collide across principals with
same-titled meetings on the same day.

**Thread reads are index-only.** `list_threads`, `read_thread` and the search's
title lane use `ChatStore::list_thread_summaries_for_prefix`, answered from the
in-memory session index without opening documents — meeting history grows with
the calendar forever.

**The thread cursor is a keyset.** `next_cursor` encodes `(created_at,
thread_id)` as `<epoch-millis>~<thread-id>` and compares positionally; the
argument proof admits that form and a bare thread id. Resolving by row lookup
would strand a pager whose cursor thread was archived, retitled or rotated.

**A thread id must live under the `meeting-` prefix**, or the binder would be a
general chat reader. `read_thread` also requires a named `session_id` to belong
to the named thread.

**Transcript cursors fail closed.** `ChatStore::get_messages_paginated` forgives
an unknown `before_id` by re-serving the newest page (right for interactive
chat, a loop for a pager). The binder uses `ChatStore::get_messages_before_exact`,
which returns `None` for an unknown cursor; the file store implements it with an
exact segment walk that reads only the returned page.

**Only plain-text turns cross the seam.** Tool cards, attachments and
task-status rows are not projected; their count is `skipped_non_text`. Rows the
transcript lane wrote are split into `speaker` + `text`; an ordinary turn with a
colon is not treated as attribution.

**Retrieval is keyword-only and says so.** `search_meeting_memory` is exact
case-insensitive substring match across takeaways, thread titles AND transcript
bodies. Each excerpt is a window around its match, not the head of the text.

**The takeaway tier is read at the path the writer writes.** The writer persists
`user.research_findings` through `normalized_user_memory_tier_name`, which strips
`user.`; the binder derives the key from the same normalizer (the nested path is
always empty).

Durations come from `started_seconds_ago` / `ended_seconds_ago` on the manager
status views, derived from a monotonic `Instant`. The binder reports each rail's
retention window (`ATTENDEE_ENDED_RETAIN_SECS` / `PASSIVE_ENDED_RETAIN_SECS`) so
a disappearing row is explained.

## `magician.meeting-control` — the control action class

Five verbs — `listen`, `join`, `pause`, `resume`, `stop` — as an owner-signed
destination contract, structurally the claims-decision sibling with its own
contract id and source entity so the two families can never validate each
other's envelopes.

**The destination is the first-party path.** `join_meeting_with_scope`
(attendee rail), `start_passive_listener` (listener rail), and the managers' own
pause/resume/stop transitions: same thread resolver, crash markers and refusal
vocabulary. A `#[cfg(test)]` **single-join-path pin** asserts over the module's
own source bytes that those are the only session-creating calls and that
`meeting_manager().join(`, `meeting_manager().spawn(`,
`passive_meeting_manager().spawn(`, `PassiveMeetingSession::new(` and
`MeetingSession::new(` never appear.

**Intent, not just authority.** Every START carries an
`AppMeetingControlGestureV1` — unique act id, frame session, expiry capped at
`APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS` (two minutes) — sealed into the proposal
digest, the owner's signed display, and the decision id. Five layers:

1. **The signature is timestamped and checked.** `decided_at_ms` is set by the
   trusted desktop at signing, covered by the signature, and must be recent by
   the destination's clock. Constraints relative to app-chosen values can be
   waited out (an app could set `issued_at_ms` a month ahead); only a signed
   clock reading closes that.
2. **The signed START is a two-minute object.** The proposal pins the gesture
   inside its lifetime and caps that lifetime.
3. **Time-dependent checks are repeated at the mutation boundary** — signature
   age, proposal lifetime, gesture freshness, authenticated scope liveness —
   against the destination's clock after registry/pairing I/O and reservation.
4. **The decision id is consumed durably.** `create_new` on a per-scope marker
   named by the decision digest, synced before proceeding, so an envelope applies
   at most once. Taken immediately before the manager call, so a refusal that
   touched nothing leaves the envelope usable. An unrecordable claim fails
   CLOSED. Markers older than the maximum proposal TTL are swept.
5. **The decision id discriminates.** It folds in the gesture *and* the sealed
   proposal digest, so gesture-less verbs on one session do not hash identically
   (pause/resume cycling must stay possible).

**Not proven: that a person was present.** `surface_session_id` is read by
frame-side code; no host registry witnesses the act. The gesture attests recency
and single-use; the owner's signature over a display showing the window carries
intent. A host-minted intent ticket would close that gap and is not built.

Pause, resume and stop carry no gesture, so a risk-reducing act never reads like
an intent-bound start in the signed display.

**Concurrency refusal, atomically.** A START is refused while any capture is
live on either rail; probe and start are ONE critical section
(`media_seam::meeting_capture_reservation`) so two signed starts cannot both see
"nothing live". The wait is bounded (a wedged join must not block every later
start). This serializes **app-initiated** starts only; first-party starts do not
take the reservation, by design.

**Every control is scope-checked.** Registries are keyed by session id alone, so
`pause`, `resume` and `stop` resolve the session's recorded owning scope and
refuse anything else, including sessions with no recorded owner. The
concurrency refusal never names the live session (a leaked id is a stop target).

**Everything is audited.** Accepted and refused, every door, one log:
`<scope>/workdirs/meeting_control_audit/<UTC date>.jsonl`. Rows carry the origin
(`first_party_api` / `app_control_destination` / `compiled_meeting_tool`), verb,
outcome, session, and — for the app door — installation, signed decision id and
gesture id. The attendee rail is audited **inside** `join_meeting_with_scope`,
which takes the origin as a required argument, so no caller can reach it without
declaring its door (including the compiled `meeting` tool's agent joins). The
listener rail is audited by its two callers (per-platform/source start variants).
Writes are best-effort and detached (`record_capture_control_detached`): a
synchronous append on the stop path would stall the reactor, and refusing a stop
because its record failed would invert the risk.

**A final fate earns one receipt, and the route publishes it.** The destination
mints one row per signed decision into
`<scope>/workdirs/meeting_control_receipts/<decision hex>.json`, only when the
fate is FINAL (envelope spent, or owner declined) and the paired desktop's
signature and its binding to current authority were proven. The ledger is
host-owned, so the route publishes the row into the package's
`control_receipt` entity as an owner mutation keyed `receipt-<decision hex>`,
reading the LEDGER, never the handler's return value — a retryable refusal
publishes nothing. One decision is one row (a second publish collides on the
record id); resubmitting a spent envelope republishes a lost row. Best-effort,
like the audit.

Route: `POST /api/magician/v2/apps/installations/{installation_id}/meeting-controls/owner-decisions`,
interactive owner sessions only, with every mutable authority input re-resolved
from the authenticated route immediately before the mutation boundary.

## The package

`magician_data_v3/system/meetings/app/` — system-class, born internal, mounted
at `/meetings-console` by nav declaration, with two bounded native widgets
(`active_capture`, `recent_meetings`) and one state indicator.

**The indicator is not the capture-visibility mechanism.** That is the
host-rendered TopBar dot (Layer-1, untouched); an app-rendered chip can never
satisfy the capture-visibility invariant. The chip selects exactly the
`capture_session` row whose `live` is true (the entity also holds ended and
other-scope rows); two simultaneous live captures hide the chip. Both widgets
declare `fallback: unavailable` — a client missing `declarative_table_v1` hides
them, since a `view` fallback needs a second declared read.

The eleven workflows are six reads over the binder and five control-request
recorders. A control workflow writes exactly one `control_request` row with
`actor_ref` null and `apply_state: recorded`, and starts nothing. Its
`payload_json` must be the canonical (key-sorted, whitespace-free) document the
destination recomputes from the signed proposal and compares byte for byte.

## Known ceiling: the live data plane

Bridge polling runs on 5–10 s bounds. The scripted-surface host allows **32
bridge messages and a 15-minute TTL per session** (`AppScriptedSurfaceWatchdog`),
bounding a 5 s console to roughly two to three minutes of live view.
`surfaces/console.js` makes the budget explicit:

- polling spends from a bounded pool and stops at a reserve, flipping the header
  chip to "live updates paused" instead of failing;
- the reserve is kept for controls, so a STOP is never blocked;
- each tick costs one message, alternating "launch the governed sync" and "read
  the projection it wrote";
- polling stops while the frame is hidden.

Lifting the ceiling needs a streaming surface primitive or a reviewed watchdog
change.

## Invariants

- Reads are executor-scoped; the binder has no mutation path, and a
  compile-time assertion refuses an action name beginning with a control verb.
- A control acts only on a session this scope owns; an unowned or unknown
  session gets one indistinguishable refusal.
- Capture visibility stays first-party: the TopBar dot and `/observe` sections
  are untouched, and an active capture is visible in both places.
- Stopping and pausing never depend on the frame being alive. The first-party
  paths remain authoritative; the console's controls are a convenience overlay.
- Join refusals fail closed exactly as the API's do. An app action can never
  soften a refusal the first-party path upholds.
- Polling is the data plane; semantic search, streaming and host-minted intent
  tickets are not built.

## Wire parity

`/meetings`, `/meetings/active`, `/meetings/listen`, `/meetings/join`,
`/meetings/{id}`, `/meetings/{id}/stop`, `/meetings/{id}/pause`,
`/meetings/{id}/resume`, `/meetings/{id}/audio` and `/meetings/upcoming` keep
their routes and response JSON. `join_meeting_with_scope`'s required `origin`
argument is a Rust signature, not a wire field; the handlers' `AgentResources`
extractor for the audit does not change the wire. Consumers: web UI, iOS
`MeetingsAPI.swift`, Android `magdroid/.../meetings`, and the compiled `meeting`
tool.
