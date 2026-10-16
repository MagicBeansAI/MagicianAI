# OPC owner and reader surfaces

Eight HTTP surface files for the One Person Company programme. Seven serve the
owner inside `/api/magician/v2`: `data_room_api`, `approval_envelopes_api`,
`engagements_api`, `counterparties_api`, `work_modules_api`, `suppression_api`
and `delivery_receipts_api`. `data_room_reader_api` serves outsiders and mounts
outside it.

Shared owner-surface rules:

- **Audience kind is a parameter**, parsed by `AudienceKind::parse`, which
  refuses unknown words instead of defaulting to `engagement`. Panels, accounts
  and people use the deal routes; `AudienceRef::as_key` keeps `engagement:acme`
  and `account:acme` distinct.
- **Actor and scope come from the boundary.** Write bodies are
  `deny_unknown_fields` with no principal, workspace or actor field; sending one
  is `400`.
- **Changed payloads are `409` naming the remedy; identical replays resume**
  (idempotent stores would otherwise ignore changed terms).
- **Terminal states never resurrect.**

## `data_room_reader_api` — the counterparty's room

`GET /rooms/{room_id}` and `GET /rooms/{room_id}/document`, at the app root
(the `/api/magician/v2` scope sits behind Cloudflare Access; counterparties are
not team members). Authority is the capability link, rechecked against the
*living* audience every request, so revoking the relationship closes the room
without touching the link. The order is the safety property: present the secret
against `share_links` (revoked, expired or no-longer-in-audience links refuse;
hash only; never logged or echoed), **record the access event** before any body
is served, then serve only what this identity may see (restricted documents
absent from the *listing*); a lapsed engagement refuses distinguishably.
Membership resolves through `counterparties`; a merged-away counterparty
resolves to the surviving id, mismatches the room's audience ref and refuses
rather than re-aiming the room. Prefer the header
secret over `?k=` (access logs, `Referer`). Details:
[data room](../magician/data-room.md#reader-surface).

## `data_room_api` — the owner's side of the same rooms

`POST /data-rooms`, `GET /data-rooms`, `GET /data-rooms/{room_id}`,
`POST|DELETE /data-rooms/{room_id}/documents`,
`POST /data-rooms/{room_id}/links`, `.../links/rotate`, `.../links/revoke`,
`POST /data-rooms/{room_id}/close`. Route-to-store map:
[data room owner surface](../magician/data-room.md#owner-surface--the-grant-path).

- Disclosure is recorded before the grant: `add_document` per
  `(document, admitted live-link holder)` before the entry is appended, link
  issue/rotate via `data_room::disclosure_bridge::record_room_disclosures`
  before the credential is minted. Failure leaves no grant
  (`disclosure_not_recorded`).
- Revoke, withdraw and close resolve no roster and record nothing, so an
  unreadable register cannot keep a leaked URL alive.
- Secrets: 32 bytes of `OsRng`, hex, returned once, stored nowhere
  (`share_links` refuses to generate; no request field supplies one). Reissue is
  fresh entropy in the same slot; closed and revoked stay so.
- Refusals: one-revision references only (`unpinned_document_reference`,
  `ambiguous_document_reference`, **400** naming the remedy, via
  `names_a_revision`, rather than the store's retryable 503);
  `expires_at` required and in the future (inclusive); off-roster identities,
  empty rosters, and unknown, unreadable or merged-away relationships get
  nothing (roster via the reader's `AudienceSource`); `open` and `add_document`
  answer `409` on changed payloads.

## `approval_envelopes_api` — what standing consent exists

`GET|POST /approval-envelopes`, `GET /approval-envelopes/{envelope_id}` (with
consumption ledger), `POST /approval-envelopes/{envelope_id}/revoke`,
`POST /approval-envelopes/preview`, over `approval_envelopes::owner_view`, where
standing is derived (never recomputed or cached in a handler, so the surface
cannot disagree with the resolver). Revocation is forward-only; grants state
scope, limits and expiry (no default). See
[approval envelopes](../magician/approval-envelopes.md#the-owner-surface).

### `POST /approval-envelopes/preview` — would this act be waived

Body: `scope_kind` + `scope_id` (`goal` | `program` | `engagement`),
`capability`, `action`, `params` (default empty), `act_ref` (required; the debit
is idempotent on it), optional `engagement_id`, `engagement_identities`,
`attachments_outside_ledger`, `value_micros`. The request is derived from
`params` (no asking about a cleaner act); empty `capability` or `action` is
refused. `preview_approval_waiver` downgrades every live posture to shadow:
`advisory: true`, no debit. `would_be_covered` is true only for `not_enforcing`
(a live envelope covers it; only the posture blocks a waiver); posture `off`
answers `ask` and names no envelope.

## Standing consent is spent, not just displayed

`approval_envelopes::waiver` sits behind `ApprovalGate::check` (sole
interpreter of `requires_approval`): a covered prompt is dropped **and
debited** in one step. The act key excludes the step id (a resume would
double-debit) and the model-authored goal text; parameters are ordered before
hashing. Posture `off` (default) waives nothing. See
[approval waiver](../magician/approval-envelopes.md#approval-waiver).

## `engagements_api` — who may act for whom

List (`GET /engagements`, live ceilings), detail (`GET /engagements/{id}`),
the cross-engagement read of one counterparty
(`GET /engagements/surface?counterparty=&program_id=`), the audience read
(`GET /engagements/{id}/audience`, label resolved against the register),
**create** (`POST /engagements`), **renew** (`PATCH /engagements/{id}`) and
**revoke** (`POST /engagements/{id}/revoke`, effective **mid-run**: the store
enforces it at the dispatch boundary), all scoped to the authenticated
principal. With no engagement there is no `EngagementAuthorityRef`: envelope
scope `None`, work bindings `Unbound`, no retrieval confinement, no maturity
acts. See [engagements](../magician/engagements.md).

### `POST /engagements` — the write path

Body is **work-shaped**: `work_context` (`{"kind": "program", "id": "..."}`),
not `program_id`; `EngagementStore::grant_for_work` derives the ceiling.
Refusals, each fail-closed: `expires_at_ms` missing (no default) or past;
ceiling entries this process cannot dispatch (registered providers, pack
definitions and their action leaves, the executor's non-pack policy names;
exact, case-sensitive; a case-only miss is `did_you_mean`, never substituted);
`delegate_to_agent` / `handover_to_agent` (bounded by `team[]`); no registry; a
counterparty label failing the register's own derivation. Retries resume the
identical live engagement; changed terms are `409` naming what differs.

### `PATCH /engagements/{engagement_id}` — renew, do not recreate

Moves the window forward on the same id (re-creating would mint a new ULID and
strand executions, pause blobs, envelopes and disclosures). Forward-only;
revoked or clock-closed refuses; an identical renewal writes nothing and keeps
the revision.

## `counterparties_api` — who we know, and how we reach them

`POST /counterparties`, `GET /counterparties`, `GET /counterparties/resolve`,
`GET /counterparties/candidates`, `GET /counterparties/{counterparty_id}`,
`GET /counterparties/{counterparty_id}/audience`,
`POST /counterparties/{counterparty_id}/identities`,
`POST /counterparties/{counterparty_id}/identities/{identity_id}/promote`.

### The decider comes from the boundary, never from the body

`created_by`, `recorded_by` and `decided_by` come from
`VerifiedRequestIdentity` (never deserialized from a payload), so
`promote_identity` can refuse a `research_inferred` address promoted by its own
recorder; unproved requests cannot write (`request_authenticated` **is** that
identity). A promotion names the **channel that carried the proof**;
`channel_is_verified` decides (SMTP and unclassified channels do not count).
`caller_claim` is `None` (a claim may only de-escalate).

### Resolution and affiliation are different routes

`GET /counterparties/resolve` returns `on_file` (the register holds the
address) and `proved` (an owner decided it reaches that organisation); **only
`proved` grants**. `GET /counterparties/candidates` is domain affiliation for
owner review (`review_candidates`; empty means nothing to review, never
cleared, resolved or permitted). `GET /counterparties/{id}/audience` takes
`audience_kind`, needs no engagement, and holds **verified addresses only**; an
empty audience admits nobody.

### Writes from the traffic paths

Neither non-owner writer can verify anybody. **Inbound:** `/chat/active` and
`/chat/new` call `chat::inbound_sender::identify_inbound_sender`, which resolves
the sender and advances `last_seen` by one `observe` line (never verification,
provenance or `first_seen`); `authority()` is `Some` for exactly one state; kind
comes from `channel_address_kind` or `kind_fixed_by_channel` (only where the
transport fixes the shape, never for a handle). **Outbound:**
`ChatService::relay_envoy_owner_decision` (from
`POST /hitl/{correlation_id}/respond`) files the owner-approved reply address,
**always unverified**, before the relay spawns; it never invents an
organisation, so an unknown one files nothing and returns as a lead. Details:
[counterparties](../magician/counterparties.md#where-it-is-used).

## `work_modules_api` — the four composable work modules

`magician-api/src/work_modules_api.rs`, under `/work`: `run_state`,
`scheduling`, `claim_manifest`, `obligations`, plus `GET /work/introducers`,
`GET /work/negotiations/open-ask`, `POST /work/runs/inbox-sweep`. One file, so
scope resolution, the separator guard, the audience parser and the
unreadable-store refusal exist once; a run never reaches into a negotiation,
nor the register into either.

### Durable runs — `/work/runs`

`GET|POST /work/runs`, `GET /work/runs/{run_id}`, `POST /work/runs/inbox-sweep`;
writes `/fields`, `/answers`, `/gaps`, `/gaps/resolve`, `/expectations`,
`/expectations/fulfil`, `/submit`. Model: [run state](../magician/run-state.md).

| Route | Contract |
| --- | --- |
| `GET /work/runs/{run_id}` | Run plus `expectations_awaiting` and `gaps_for_owner`; wait ages derived on read. |
| `/gaps/resolve` | Requires `resolved_by`; reply ref goes in `evidence_refs`. |
| `/expectations/fulfil` | Takes an already-extracted event (reads no inbox); answers `fulfilled`, `already` or `unmatched`, not `404`. |
| `/submit` | Records the covering outward act on the `Form` channel **first**, then seals; `submitted` needs a named person and payload artifact ref. |
| `GET /work/runs` | `RunStateStore::all_runs` (ids derive from `(scope, purpose, resource_ref)`). |
| `POST /work/runs/inbox-sweep` | Body: optional `workspace`. Reads the `run_inbox` mailbox (`magician_config.run_inbox`); unset is **503** `run_inbox_no_source`, not an empty sweep. Consumes or moves nothing; run-store idempotency on the event ref is the cursor. Reports `examined`, `accounted_for`, `runs_waiting` and outcomes `fulfilled`, `already_fulfilled`, `unmatched`, `ambiguous`, `refused`. A source several runs wait on closes **neither**; one bad run blocks no others. |

### Time negotiations — `/work/negotiations`

`GET|POST /work/negotiations`, `GET /work/negotiations/{id}`,
`GET /work/negotiations/open-ask`, `/reply`, `/re-offer`, `/hold`,
`/reschedule`, `/close`. Model: [scheduling](../magician/scheduling.md).
Three more routes are registered here but handled in
`scheduling_work_api.rs`: `POST /work/negotiations/inbound` (exact inbound
association of a reply), `POST /work/negotiations/{id}/bind-offer`, and
`GET /work/follow-through` (bounded owner read for the Work surface).

- **No channel is named.** `offer_message` returns words *and* times (caller
  sends, then returns the act ref); `hold_intent` books nothing. Both return the
  intent or a `refusal`, never an omission.
- `/hold` reads the slot off the acceptance, not the request; `/re-offer`
  records a changed offer, which `open` cannot.
- `GET /work/negotiations/open-ask`: by `audience_kind` + `audience_id`, or
  `channel` + `channel_address` (optional `channel_address_kind`,
  `channel_verified`, de-escalate only; proofs as `/chat/active`). Answers
  `one_open_ask` (absorb against `negotiation_id`), `no_open_ask` (not an
  error), `ambiguous_open_asks` (ids returned; caller chooses); reads no reply
  and absorbs nothing. Registered before `/work/negotiations/{negotiation_id}`;
  derived ids (`neg-<hash>`) are never `open-ask`.
- `SchedulingStore::all_negotiations` backs the unfiltered listing and sweep
  (logs are named by audience-key hash).

### Claim manifests — `/work/claim-manifests`

`POST` binds a revision's claim set; `GET ?artifact_ref=[&revision_ref=]` reads
claim history; `GET /carrying?claim_ref=[&latest_only=]` is the
**correction-propagation query** (what still carries a corrected claim). Claims
per revision are immutable: an identical (order-insensitive) rebind resumes, a
different one is `409`; the fix is a new revision. See
[claim manifest](../magician/claim-manifest.md).

### The register — `/work/obligations`

`GET` answers what is owed and lapsed, directions counted apart. `POST` records
a promise; `POST /{id}/settle` closes it `met` or `released` (release needs a
reason). `POST /work/obligations/sweep` runs the data-room follow-up and
scheduling silence sweeps now (on-demand twin of `obligation_sweeps::worker`).
See [`docs/components/magician/obligations.md`](../magician/obligations.md).

`GET` also joins counterparty behaviour (`obligation_sweeps::attention`
reading, joined by `explain_register` on `obligation_id`: row `reading`,
response `readings`) on this route, not a second queue, so one source says
what is outstanding. Fail-closed: unknown handles go under
`readings.row_not_in_register`, unraised under `readings.not_yet_raised`, an
empty register attaches nothing; unreadable rooms give
`readings.available: false`, never zeros, while the register is still
answered. Readings are derived on demand, never recorded (their visit count
must stay out of the identity tuple), and use `WorkModulesApi`'s
`ObligationSweepConfig` (the worker's own type, set by `with_sweep_config`),
whose windows shape `due_at` and the id; the response states them.

### Introducer debts — `/work/introducers`

Introducers owed an update. **Proposes, never records** (agree via
`POST /work/obligations`). Query: optional `workspace`,
`update_after_days` (default 30, refused outside 1–3650). Reply:
`introductions` and `introducers` denominators; `owed` lists those past the
window with a `proposed_obligation`. An unreadable register fails the request
rather than returning a partial graph.

### Refusals that shape every handler

- **`U+001F` is refused at the door** (`guard_id_component`, before any store):
  stores join caller strings with it to derive ids, so it could fuse identities
  across tenants.
- **Unreadable store: `500`, never `[]`.** Missing run, negotiation or
  obligation: `404`, never an empty list. A store refusal: `409` in its own
  words (not ready to submit, slot never offered).
- **No route derives a time state** (lapsed, silent, awaiting, ready); modules
  do, from the handler's instant.

## `suppression_api` — and the delivery routes beside it

`magician-api/src/suppression_api.rs`, mounted by
`configure_suppression_routes` (once, from `magician-bin/src/main.rs`).
Register: `GET|POST /suppressions`, `GET /suppressions/check`,
`POST /suppressions/lift`. Delivery routes reading the
`magician_v2::delivery_hygiene` ledger:

| route | question |
|---|---|
| `GET /delivery/unacknowledged` | what did we send that nothing came back for |
| `GET /delivery/watch` | is anything asking that on a cadence |
| `POST /delivery/reindex-work-axes` | re-file acts under the work they already name |
| `POST /delivery/receipts` | **record what a provider said about one act** |
| `GET /delivery/receipts` | what is on file for one act, and who put it there |

`delivery_receipts_api` (`POST /delivery/sent`, `POST /delivery/receipts/mail`)
mounts beside these via `configure_delivery_mail_routes`. Ledger semantics:
[delivery](../magician/delivery.md).

### `GET /delivery/unacknowledged`

`DeliveryLedger::unreconciled`: sends with no receipt (a silently broken
integration). Counts with denominator, overdue acts oldest first, per-rail
counts; `grace_hours` defaults to 24, echoed. Derived on read; the read records
nothing. Never a false green zero: `any_dispatch_recorded: false` plus `quiet`
separate "never dispatched" from "all confirmed"; an unreadable dispatch log is
**503**, never an empty 200; unnameable rails count under `unattributed`, never
a named rail. The rail is the act's `OutwardChannel`, read from the owning
disclosure (not the dispatch row) and attributed via a **supplied map**.

### `GET /delivery/watch`

Health of `delivery_hygiene::worker::SuppressionSweepWorker` (whose tick also
reads silence for every configured scope); unattached is **503**
`delivery_watch_not_wired`. `state`: are suppressions being recorded.
`silence_state`: `degraded` when overdue acts reach the floor (default 1,
naming the worst rail), `no_dispatches` (never `idle`) when no scope has
dispatched.

### `POST /delivery/reindex-work-axes`

Body: optional `workspace`. Re-files outward acts under the work axes their
records name, so the sweeps can reach programme/engagement history; it
**re-indexes, never reconstructs** (an act naming no work stays
`unattributed`), so check `unattributed` and `unattributed_is_not_repairable`,
not the 200. Also returns `acts_seen`, `engagement_entries`,
`program_entries`, `already_indexed`.

## `POST /delivery/receipts` — the door a receipt comes through

The production caller of `DeliveryLedger::reconcile` (otherwise live sends stay
`dispatch_unknown`). The handler holds no store logic: it proves the caller,
takes scope from that proof and passes the scope's dispatch log and receipt to
`delivery::intake::ReceiptIntake::admit`, which hands it to `reconcile`
untouched. The response gives disposition (`opened` / `replayed` /
`advanced` / `superseded`), what held when order refused, identity and act
state, and the `suppression_cause` hygiene will act on. Provider bridges, a
Kapso adapter, an SMTP reconciler and owner transcriptions POST the same body;
`source` (`provider` or `operator`, required) records the route without
changing judgement; operators add a `payload_ref`.

### Authentication is the whole risk

An open receipt endpoint would be a **remote suppression primitive**
(`complained` and hard bounces become entries only an owner act with evidence
can lift; `delivered` hides a failure). Every `/api/magician/v2` route is behind
`verify_access_middleware` (bypassed only by `POST /devices/enrollment/exchange`
and `POST /devices/apps-automation/enrollment/exchange`),
which verifies no provider signatures, and the Node webhook receivers carry no
delivery stream, so both receipt routes, read (it names who recorded each
receipt) and write, are **owner-authenticated only**: `VerifiedRequestIdentity`
(Access, paired device or real loopback peer, the `apps_api` gate) with scope
from it, never `X-Principal` / `X-Workspace` (a disagreeing header is **403**).
Workspace-bound identities (paired device, loopback) use their own; an
interactive Access owner must name one (400 if absent). Loopback is
`anonymous`/`default` (`trusted_loopback_request_identity`), so an unattended
local bridge reconciles only that scope. What a
provider-direct webhook would need:
[delivery](../magician/delivery.md#authentication-and-what-a-provider-direct-door-would-still-need).

### Refusals

- `admit` refuses acts absent from the scope's dispatch log (no dispatches at
  all is its own refusal: wrong scope or unwritten log).
- `state` (off `DeliveryState::ALL`) and `source` have no defaults: `accepted`
  would fake an acknowledgement, `provider` would misfile a transcription.
- `deny_unknown_fields`, no principal, workspace or actor field.

### `GET /delivery/receipts` — including refused attempts

Observations plus every attempt; the attempt log is written **before**
reconcile, so refused attempts still name their author; it carries no
disposition (that lives only in the ledger). `dispatch_recorded: false` is
reported, not refused.

## `delivery_receipts_api` — the mail door

`magician-api/src/delivery_receipts_api.rs`, mounted by
`configure_delivery_mail_routes`.

| route | question |
|---|---|
| `POST /delivery/sent` | register what left, under the identifier a bounce will quote back |
| `POST /delivery/receipts/mail` | hand in one raw RFC 5322 message; correlated DSN recipients become receipts |

No wired provider emits an email delivery event stream; hard bounces arrive as
RFC 3464 DSNs. `provider` is a parameter, so an inbox reader, an SMTP bounce
mailbox and an owner pasting a forwarded bounce use the same calls; each
correlated receipt goes through `ReceiptIntake::admit`. Caller proof restates
`suppression_api::receipt_caller`
(scope from identity, disagreeing header **403**, unproved **401**). App state
is the `SuppressionApi` workspace handle.

### `POST /delivery/sent` — how a report will name this send

Records how a report will name an act, possibly later than the dispatch (a
rail may learn its message id only from the provider). Does **not** require a
dispatch-log entry
(reported as `dispatch_recorded`; `admit` still refuses undispatched receipts).
Body (`deny_unknown_fields`): `act_ref`, `provider`, `message_id` (RFC 5322,
brackets optional) and/or `envelope_id` (RFC 3461), `audience` (non-empty;
bounce addresses are checked against it), `sent_at`. Identical registration is
one row; a different act, audience or instant under one identifier is refused.

### `POST /delivery/receipts/mail` — one message, through the same intake

Body (`deny_unknown_fields`): `provider`, `payload_ref`, `raw_mail` (raw
RFC 5322, ≤ 1 MiB),
`source` (`provider` | `operator`, required), `dry_run` (default false, returns
`would_record`). Ordinary mail: **200** `recognised: false`; unreadable
self-declared report: **422**; store fault: **503**. Uncorrelatable bounces are
recorded nowhere and listed under `refused` (a guess could permanently suppress
the wrong person). A duplicate returns `replayed`;
changing `payload_ref` for the same message is an error.

## Why HTTP readers pass `RetrievalScope::Unbound`

Engagement-scoped retrieval confines **executions** carrying engagement
authority (unlabelled context is refused). Owner memory reads over HTTP
(analytics memory views, the VibeDev rail) pass `RetrievalScope::Unbound`
explicitly, with a comment saying why: **the owner inspecting their own memory is the one reader an engagement boundary
is not drawn against.** The comment marks these as deliberate, unlike a silent
`Unbound` default.
