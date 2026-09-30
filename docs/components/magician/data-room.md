# Data room — the document channel of an engagement

Plan: `docs/archive/plans/2026-08-07-opc-deal-close.md`.

The container, audit log, follow-up sweep, and disclosure bridge live in
`magician-learning/src/data_room/`. The owner grant surface is
`magician-api/src/data_room_api.rs` (mounted inside `/api/magician/v2`). The
reader is `magician-api/src/data_room_reader_api.rs` (mounted at the app root,
outside Cloudflare Access). Obligation composition is
`magician-media/src/obligation_sweeps/`.

A data room is the document channel of a relationship: who may read, when
access ends, and what the audit target is all inherit from the audience. The
learning crate holds **no roster of its own** and mints no token; HTTP layers
open rooms, issue links, and serve readers.

## A channel, not a subsystem

The room binds to an [`AudienceRef`](audience.md), not an engagement id. The
same container serves a deal room, a cohort's materials, a client's
deliverables, an audit pack, or one person's records. The audience **kind is
part of the room's identity**, so the same company as a live deal and as a
standing client get different rooms.

`visible_to` requires the room to be open **and** the audience to be current.
A roster handed in from somewhere else — matching identities, wrong
relationship — grants nothing.

## References, never copies

A room holds `artifact_ref`s pinned as `artifact_ref@revision`. `add_document`
refuses a reference that names no revision, and refuses one carrying more than
one `@` (so `mailto:a@b.test` is not silently “pinned” to a revision that
matches nothing). The owner surface answers those as **400** with the remedy,
ahead of the store; the store's `anyhow::Error` would otherwise become a 503
telling the caller to retry the one thing that can never succeed.

Rooms written before this guard may still hold either shape. Reads report them
apart rather than guessing.

## Properties

**One audience, one room.** The id derives from the audience, so re-opening
returns the existing room.

**A withdrawn document leaves the room but not the record.** The entry is kept
and marked. Adding is idempotent; re-adding a withdrawn document restores it.
The earlier withdrawal stays in the log.

**Standing is derived, never stored.** Expiry is inclusive. A deliberate close
outranks expiry. A closed or expired room takes no documents. A closed room
shows nobody anything: `visible_to` checks standing *before* per-document
rules.

**`Everyone` means every identity in the audience — never the public.** There
is no “anyone with the link” variant.

**Per-document identities are not validated at write time.** `permits` checks
audience membership on every read, so naming a stranger grants nothing.

## Owner surface — the grant path

`magician_api::data_room_api`, mounted **inside** `/api/magician/v2`. Production
callers of `DataRoomStore::open` and `ShareLinkStore::issue` live here.

| Route | What it reaches |
| --- | --- |
| `POST /data-rooms` | `DataRoomStore::open` |
| `GET /data-rooms` | `list`, or `for_audience` when the audience is named |
| `GET /data-rooms/{room_id}` | `load` plus `ShareLinkStore::for_resource` |
| `POST /data-rooms/{room_id}/documents` | `add_document` with a live `GrantDisclosure` |
| `DELETE /data-rooms/{room_id}/documents?ref=…` | `withdraw_document` |
| `POST /data-rooms/{room_id}/links` | `ShareLinkStore::issue` |
| `POST /data-rooms/{room_id}/links/rotate` | `rotate` |
| `POST /data-rooms/{room_id}/links/revoke` | `revoke` |
| `POST /data-rooms/{room_id}/close` | `close` |

Every route that names a relationship takes the audience **kind** as a
parameter, parsed by `AudienceKind::parse` (refuses an unknown word rather
than defaulting one).

**Recording the disclosure fails the grant.** Adding a document records one
act per `(document, admitted holder)` *before* the entry is appended. Issuing
or rotating a link calls `record_room_disclosures` *before* the credential is
minted. A failed recording leaves no grant; the refusal code is
`disclosure_not_recorded`.

**Removals never depend on what grants depend on.** Revoke, withdraw, and close
resolve no roster and record no disclosure.

**The secret is minted here and stored nowhere.** 32 bytes of `OsRng`, hex,
returned once. `share_links` refuses to generate secrets; callers cannot
supply one. A revoked hash stays dead.

## Reader surface

Mounted at the **app root** (`GET /rooms/{room_id}`,
`GET /rooms/{room_id}/document`) in `magician-bin/src/main.rs`, outside the
Access-wrapped `/api/magician/v2` scope. Authority is the presented capability
link.

The reader presents the secret against the living audience through
`ShareLinkStore::present`, writes an `AccessEvent` through
`AccessStore::record_access` **before** anything is served, and returns what
`DataRoom::visible_to` allows for that identity. Audience membership resolves
through `CounterpartyStore::audience_for`, which exposes only **verified**
identities.

Each request is a refusal point in order: malformed scope/room id (including a
`U+001F` that would fuse derived ids), then the credential against the living
audience, then the access record, then `visible_to`, then both clocks. A
lapsed relationship answers `410` with a message to ask a person rather than
`404` (a reader who thinks the link is broken retries it). The secret is taken
from `X-Room-Key` or the `k` query parameter and is never logged or echoed.

A visit that cannot be recorded is never served (`503`, empty body).

## The audit log

The log is the only signal in this set that reports what the counterparty did.
`access_store.rs` is the durable lane; judgement stays in `access_log`.

### Possession, never proof of identity

A capability URL proves **possession of the link**. The field is
`token_issued_to`. There is no field called `identity`.

### Visits, not clicks

`signal()` derives from **visits**, not event count. One visit that views the
index and then opens a document produces two events and is still one visit.
`presentations` is kept alongside.

The reader surface is the only writer of `AccessEvent`. It presents the
credential on every request but does **not** write `Presentation::sequence`
into the log: `GET /rooms/{room_id}` takes the token's next number from the
lane, and a document fetch carries the number of the opening it followed. A
reader who opens a room once and reads four documents is one visit and five
presentations. Two *openings* are two visits.

### Three states, not four

Never opened, opened once, opened repeatedly. The fourth plan state —
opened once **then silence** — needs reply knowledge the room does not have.
`is_partial()` is orthogonal: true when some of the room was read and some
was not, and **false when nothing was opened**.

### Never-opened earns the feature

A room shared and never opened is the first evidence the system can produce
that a message did not arrive. `attention_across` takes the **shared-with
list** rather than deriving it from events. Never-opened sorts first;
`is_delivery_question()` marks it.

### Bounded honestly

- Unopened means unopened out of what is *in the room*. A withdrawn document
  is not something they failed to read.
- Dwell is best-effort, `None` by default.
- `user_agent_class` is coarse: desktop, mobile, unknown.
- Revocation is forward-only. Closing prevents future access; it does not
  recall a downloaded file.
- No pixels, no beacons. `ROOM_LOGGING_NOTICE` lives next to what it
  describes.

## Follow-ups and the cycle note

`follow_ups::derive_follow_ups` turns attention into obligations: opened-then-
silent is a follow-up; never-opened is a delivery question. Both are
`OwedByUs`. A token that replied produces nothing.

`due_at` is the ripening instant (shared-at or last-seen plus the policy
window), never the sweep clock. Identity-bearing text carries the stable room
id. `next_review_at(…, now)` returns the earliest instant something **new**
could ripen, strictly in the future.

`sweep_follow_ups` pairs what would be raised with what `follow_ups_to_settle`
would release, each already carrying `obligations::obligation_id_for`.
`apply_follow_up_sweep` records **then** settles. It refuses a token that had
a *ripened* obligation in the previous snapshot but is missing from the
current one — a short snapshot must not release the whole register.

`magician_media::obligation_sweeps` builds each room's snapshot from the
share-link roster and the access lane (`snapshot_room`), calls
`sweep_follow_ups`, and writes with `apply_follow_up_sweep`. Reachable from
`POST /api/magician/v2/work/obligations/sweep` and
`obligation_sweeps::worker::ObligationSweepWorker`. The worker walks the same
rooms `sweep_scope` walks through the same `snapshot_room` assembly — one
roster, so a note cannot be built from a view the sweep never saw. Every token
enters the snapshot unanswered: the room has no view of mail, phone, or
meetings. At worst it raises a follow-up on somebody who answered elsewhere;
an owner settles that in one act.

`cycle::attention_notes` is not a second queue. Each note carries the
`obligation_id` of the register row it explains, obtained by running
`derive_follow_ups` and `obligation_id_for` — the sweep's own derivations.
`obligation_sweeps::attention` composes notes for a whole scope;
`explain_register` joins them onto the rows the caller already loaded.
`GET /api/magician/v2/work/obligations` serves the result: a row carries a
`reading` field, and the response carries a `readings` block. A handle the
register does not hold is reported under `readings.row_not_in_register`, which
is what a sweep that has not run — or one that ran under different waiting
windows — looks like. The route states the windows it read under because
`due_at` is part of the derived id.

`AttentionReading` has `never_opened`, `read_then_silent`,
`returned_then_silent`, and `answered`, because reply knowledge has been
joined in by then. It also carries the visit count, the documents opened, the
documents never reached, and a deterministic `headline` — presentation only,
never recorded. `obligation_id` is `None` when they replied, the window has
not closed, or the room recorded a visit with no time. Never-opened notes
sort first.

## Disclosure bridge

`disclosure_bridge::record_room_disclosures` records one outward act per
(present document, link holder) when visibility is granted — prepare-before-
access, channel `Room`, class confidential disclosure. Acts advance to
provider-accepted so `is_active_disclosure()` is true. The bridge takes live
link holders as a plain identity slice from `share_links`, so neither module
imports the other.

## Not built here

- Per-identity visibility in earnest (the type records `Identities` now;
  selection is still the audience plus that list).
- An engagement store. `visible_to` takes the roster as an argument. The
  reader supplies it from `CounterpartyStore::audience_for`.
