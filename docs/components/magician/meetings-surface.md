# Meetings surface — the console app over the meeting rails

The meetings console as an app: live capture state and transcripts, thread
history with keyword search, takeaways, upcoming calendar context and the full
capture controls. It is **additive alongside first-party `/observe`**: its
meetings sections, the TopBar capture dot and the fast-poll lease are
untouched. The package is a new consumer of unchanged routes, never a route
move. Rollback is **disable**, never uninstall.

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
  /meetings/upcoming` and the binder both call it. The cache is keyed by the
  scope's capability auth root, so one scope's calendar is never served to
  another.
- The process chat store is published once at boot
  (`chat::storage::publish_global_chat_store`) so the binder shares the
  server's index. It is read-only by contract; mutations go through the
  store's owner.

## `meetings_data` — the read binder

Same pattern as the `evidence_data` / `thinking_maps_data` binders: closed
action set, fail-closed argument proofs, executor-owned scope (`__principal` /
`__workspace` are injected; public values are assertions that never switch
scope), fixed scan budgets, 8 KiB input and 512 KiB result ceilings, 30-second
timeout.

| Action | Shape |
| --- | --- |
| `active_session {}` | Both capture registries as one bounded page. |
| `list_threads {status?, text?, limit?, after_thread_id?}` | The scope's dated `meeting-` threads, newest first. |
| `read_thread {thread_id, session_id?, before_message_id?, limit?}` | One transcript page, backwards from newest. |
| `read_takeaways {thread_id?, limit?}` | The meeting rows of `user.research_findings`. |
| `upcoming_meetings {}` | The scope's cached next ~12 h of calendar meetings. |
| `search_meeting_memory {text, limit?}` | Bounded keyword retrieval across all three. |

**The registries are process-global; the reads around them are not.**
`active_session` shows every session on both rails, because whether capture is
running must never be hidden (same as `GET /meetings/active`). `session_id`,
`mode`, `status`, `live`, `paused` and durations are registry-wide;
`thread_id`, `title` and `url` appear only when `in_scope` (the registry
recorded this scope as owner at spawn), since a title or link identifies a
room. The first-party `latest_summary` is dropped. A cross-scope row is
visible but useless, by intent.

Ownership is the recorded scope, not chat-index membership: a meeting's chat
session is created lazily (the owner's capture would read as foreign), and
thread ids `meeting-<slug(title)>-<date>`
collide across principals with same-titled meetings on the same day.

**Thread reads are index-only.** `list_threads`, `read_thread` and the
search's title lane use `ChatStore::list_thread_summaries_for_prefix`, served
from the in-memory index without opening documents.

**The thread cursor is a keyset.** `next_cursor` encodes `(created_at,
thread_id)` as `<epoch-millis>~<thread-id>`, compared positionally; the
argument proof admits that
form and a bare thread id. A row-lookup cursor would strand a pager whose
thread was archived, retitled or rotated.

**A thread id must live under the `meeting-` prefix**, or the binder would be
a general chat reader. `read_thread` also requires a named `session_id` to
belong to the named thread.

**Transcript cursors fail closed.** `ChatStore::get_messages_paginated`
re-serves the newest page for an unknown `before_id` (a loop for a pager), so
the binder uses `ChatStore::get_messages_before_exact`, which returns `None`;
the file store does an exact segment walk that reads only the returned page.

**Only plain-text turns cross the seam.** Tool cards, attachments and
task-status rows are counted in `skipped_non_text`. Rows from the transcript
lane split into `speaker` + `text`; an ordinary turn containing a colon is not
treated as attribution.

**Retrieval is keyword-only and says so.** `search_meeting_memory` is exact
case-insensitive substring match over takeaways, thread titles and transcript
bodies. Each excerpt is a window around its match.

**The takeaway tier is read where the writer writes.** The writer persists
`user.research_findings` through `normalized_user_memory_tier_name` (which
strips `user.`); the binder uses the same normalizer.

Durations come from `started_seconds_ago` / `ended_seconds_ago` on the manager
status views (monotonic `Instant`). The binder reports each rail's retention
window (`ATTENDEE_ENDED_RETAIN_SECS` / `PASSIVE_ENDED_RETAIN_SECS`) so a
disappearing row is explained.

## `magician.meeting-control` — the control action class

Five verbs (`listen`, `join`, `pause`, `resume`, `stop`) as an owner-signed
destination contract. It is shaped like the claims-decision sibling but has its
own contract id and source entity, so neither can validate the other's
envelopes.

**The destination is the first-party path.** `join_meeting_with_scope`
(attendee rail), `start_passive_listener` (listener rail) and the managers'
own pause/resume/stop: same thread resolver, crash markers and refusal
vocabulary. A `#[cfg(test)]` **single-join-path pin** asserts over the
module's source that those are the only session-creating calls and that
`meeting_manager().join(`, `meeting_manager().spawn(`,
`passive_meeting_manager().spawn(`, `PassiveMeetingSession::new(` and
`MeetingSession::new(` never appear.

**Intent, not just authority.** Every START carries an
`AppMeetingControlGestureV1` (unique act id, frame session, expiry capped at
`APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS`, two minutes) sealed into the
proposal digest, the owner's signed display and the decision id:

1. **The signature is timestamped.** `decided_at_ms` is set by the trusted
   desktop at signing, covered by the signature, and must be recent by the
   destination's clock. Checks against app-chosen times can be waited out (an
   app could set `issued_at_ms` a month ahead); only a signed clock reading
   closes that.
2. **The signed START lives two minutes.** The proposal pins the gesture
   inside its lifetime and caps that lifetime.
3. **Time checks repeat at the mutation boundary**: signature age, proposal
   lifetime, gesture freshness and scope liveness, on the destination's clock
   after registry/pairing I/O and reservation.
4. **The decision id is consumed durably.** `create_new` on a per-scope marker
   named by the decision digest, synced before proceeding, so an envelope
   applies at most once. It is taken just before the manager call, so a
   refusal that touched nothing leaves the envelope usable. An unrecordable
   claim fails closed. Markers older than the maximum proposal TTL are swept.
5. **The decision id discriminates.** It folds in the gesture and the sealed
   proposal digest, so gesture-less verbs on one session differ and
   pause/resume can cycle.

**Not proven: that a person was present.** `surface_session_id` is read by
frame-side code; no host registry witnesses the act. The gesture attests
recency and single use; the owner's signature over a display showing the
window carries intent. A host-minted intent ticket would close the gap and is
not built.

Pause, resume and stop carry no gesture, so a risk-reducing act never looks
like an intent-bound start in the signed display.

**Concurrency refusal, atomically.** A START is refused while any capture is
live on either rail. Probe and start are one critical section
(`media_seam::meeting_capture_reservation`), so two signed starts cannot both
see "nothing live". The wait is bounded. Only **app-initiated** starts take
the reservation; first-party starts do not, by design.

**Every control is scope-checked.** Registries are keyed by session id alone,
so `pause`, `resume` and `stop` resolve the session's recorded owner and refuse
anything else, including sessions with no owner. The concurrency refusal never
names the live session (a leaked id is a stop target).

**Everything is audited.** Accepted and refused, every door, one log:
`<scope>/workdirs/meeting_control_audit/<UTC date>.jsonl`. Rows carry origin
(`first_party_api` / `app_control_destination` / `compiled_meeting_tool`),
verb, outcome, session and, for the app door, installation, signed decision id
and gesture id. The attendee rail is audited inside `join_meeting_with_scope`,
which takes the origin as a required argument, so every caller (including the
compiled `meeting` tool) declares its door. The listener rail is audited by its
two callers. Writes are best-effort and detached
(`record_capture_control_detached`): a synchronous append would stall the stop
path, and refusing a stop because its record failed would invert the risk.

**A final fate earns one receipt, and the route publishes it.** The
destination writes one row per signed decision to
`<scope>/workdirs/meeting_control_receipts/<decision hex>.json`, only when the
fate is final (envelope spent, or owner declined) and the paired desktop's
signature and its binding to current authority were proven. The route
publishes that ledger row (never the handler's return value) into the
package's `control_receipt` entity as an owner mutation keyed
`receipt-<decision hex>`, so a retryable refusal publishes nothing. One
decision is one row (a second publish collides on the record id); resubmitting a spent envelope republishes a lost row.
Best-effort, like the audit.

Route: `POST /api/magician/v2/apps/installations/{installation_id}/meeting-controls/owner-decisions`,
interactive owner sessions only, with every mutable authority input
re-resolved from the authenticated route just before the mutation boundary.

## The package

`magician_data_v3/system/meetings/app/` is system-class, born internal, mounted
at `/meetings-console` by nav declaration, with two bounded native widgets
(`active_capture`, `recent_meetings`) and one state indicator.

**The indicator is not the capture-visibility mechanism.** That is the
host-rendered TopBar dot; an app-rendered chip can never satisfy the
capture-visibility invariant. The chip selects the one `capture_session` row
whose `live` is true; two simultaneous live captures hide it. Both widgets
declare `fallback: unavailable`, so a client without `declarative_table_v1`
hides them (a `view` fallback would need a second read).

The eleven workflows are six binder reads and five control-request recorders.
A control workflow writes one `control_request` row with `actor_ref` null and
`apply_state: recorded`, and starts nothing. Its `payload_json` must be the
canonical (key-sorted, whitespace-free) document the destination recomputes
from the signed proposal and compares byte for byte.

## Known ceiling: the live data plane

Bridge polling runs on 5–10 s bounds. The scripted-surface host allows **32
bridge messages and a 15-minute TTL per session**
(`AppScriptedSurfaceWatchdog`), about two to three minutes of live view at 5 s.
`surfaces/console.js` makes the budget explicit:

- polling spends from a bounded pool and stops at a reserve, flipping the
  header chip to "live updates paused" instead of failing;
- the reserve is kept for controls, so a STOP is never blocked;
- each tick costs one message, alternating "launch the governed sync" and
  "read the projection it wrote";
- polling stops while the frame is hidden.

Lifting the ceiling needs a streaming surface primitive or a reviewed watchdog
change.

## Invariants

- Reads are executor-scoped; the binder has no mutation path, and a
  compile-time assertion refuses an action name starting with a control verb.
- A control acts only on a session this scope owns; an unowned or unknown
  session gets one indistinguishable refusal.
- Capture visibility stays first-party: the TopBar dot and `/observe` sections
  are untouched, and an active capture shows in both.
- Stopping and pausing never depend on the frame being alive; the first-party
  paths stay authoritative and the console is a convenience overlay.
- Join refusals fail closed exactly as the API's do; an app action can never
  soften one.
- Polling is the data plane; semantic search, streaming and host-minted intent
  tickets are not built.

## Wire parity

`/meetings`, `/meetings/active`, `/meetings/listen`, `/meetings/join`,
`/meetings/{id}`, `/meetings/{id}/stop`, `/meetings/{id}/pause`,
`/meetings/{id}/resume`, `/meetings/{id}/audio` and `/meetings/upcoming` keep
their routes and response JSON. `join_meeting_with_scope`'s required `origin`
is a Rust argument, not a wire field, and the handlers' `AgentResources`
extractor for the audit does not change the wire. Consumers: web UI, iOS
`MeetingsAPI.swift`, Android `magdroid/.../meetings`, and the compiled
`meeting` tool.
