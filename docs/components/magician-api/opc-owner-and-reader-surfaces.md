# OPC owner and reader surfaces

Eight HTTP surface files for the One Person Company programme. Seven serve the
owner and mount inside `/api/magician/v2`: `data_room_api`,
`approval_envelopes_api`, `engagements_api`, `counterparties_api`,
`work_modules_api`, `suppression_api`, and `delivery_receipts_api`. The eighth,
`data_room_reader_api`, serves people outside the company and does not.

Shared rules across the owner surfaces:

- **The audience kind is a parameter**, parsed by `AudienceKind::parse`, which
  refuses unknown words rather than defaulting to `engagement`. A panel, account
  or person uses the same routes as a deal; `AudienceRef::as_key` keeps
  `engagement:acme` and `account:acme` distinct.
- **Actor and scope come from the boundary, never the body.** Write bodies are
  `deny_unknown_fields` and carry no principal, workspace or actor field;
  sending one is a `400`, not a silently ignored field.
- **Changed payloads are errors, identical replays resume.** Idempotent stores
  would quietly ignore changed terms, so surfaces compare first and answer `409`
  naming the remedy.
- **Terminal states never resurrect.**

## `data_room_reader_api` — the counterparty's room

`GET /rooms/{room_id}` and `GET /rooms/{room_id}/document`.

**Mounted at the app root, outside `/api/magician/v2`**, because that scope is
wrapped in Cloudflare Access and counterparties are not team members. Authority
is the presented capability link, re-checked against the *living* audience on
every request, so revoking the relationship closes the room without touching
the link.

The request order is the safety property:

1. **Present** the secret against `share_links` — revoked, expired or
   no-longer-in-audience links refuse. The store keeps only a hash; the secret
   is never logged or echoed.
2. **Record the access event first**, before any body is served.
3. **Serve only what this identity may see.** Restricted documents are absent
   from the *listing*, not merely unfetchable (naming them is a disclosure).
4. A room whose engagement has lapsed refuses distinguishably, so the reader
   knows to ask.

Audience membership resolves through `counterparties`. A merged-away
counterparty resolves to the surviving id, which then mismatches the room's
audience ref and refuses rather than silently re-aiming the room. Prefer the
header form of the secret; `?k=` can land in access logs and `Referer`.

## `data_room_api` — the owner's side of the same rooms

`POST /data-rooms`, `GET /data-rooms`, `GET /data-rooms/{room_id}`,
`POST|DELETE /data-rooms/{room_id}/documents`,
`POST /data-rooms/{room_id}/links`, `.../links/rotate`, `.../links/revoke`, and
`POST /data-rooms/{room_id}/close`. Mounted inside `/api/magician/v2`.

**Recording the disclosure gates the grant.** `add_document` records one act per
`(document, admitted live-link holder)` *before* the entry is appended. Issuing
or rotating a link calls `data_room::disclosure_bridge::record_room_disclosures`
*before* the credential is minted (every document becomes visible to that
identity the instant the link exists). A failed recording leaves no grant, with
refusal code `disclosure_not_recorded`: a grant whose record failed is an
unaccountable disclosure.

**Removals never depend on what grants depend on.** Revoke, withdraw and close
resolve no roster and record no disclosure, so an unreadable register can never
keep a leaked URL alive.

Refusals, each fail-closed:

- **A document reference must name exactly one revision**
  (`unpinned_document_reference`, `ambiguous_document_reference`, **400**, naming
  the remedy). The surface uses the store's own `names_a_revision`; refusing here
  avoids the store's error surfacing as a retryable 503.
- **No expiry, no link.** `expires_at` is required and must be in the future
  (inclusive). A possession grant that never lapses is a standing yes to whoever
  holds the URL.
- **An identity the roster does not name gets no credential** (it would come
  alive the day someone was added). An empty roster admits nobody.
- **Unknown, unreadable and merged-away relationships refuse.** The roster
  resolves through the reader's own `AudienceSource`, reused so the two surfaces
  cannot disagree about membership.
- `open` and `add_document` answer `409` on changed payloads (moved closing date,
  narrowed visibility); identical replays resume.
- A closed room does not re-open; a revoked credential stays revoked, and a
  re-issue is fresh entropy in the same grant slot.

**The secret is minted here and stored nowhere.** `share_links` refuses to
generate secrets, so this layer mints 32 bytes of `OsRng`, hex, returned once.
There is no request field for supplying one, so a killed credential can never be
re-armed from outside.

## `approval_envelopes_api` — what standing consent exists

`GET|POST /approval-envelopes`, `GET /approval-envelopes/{envelope_id}` (with
the full consumption ledger), `POST /approval-envelopes/{envelope_id}/revoke`,
and `POST /approval-envelopes/preview`, over the transport-free projection
`approval_envelopes::owner_view`. Standing is **derived** in that projection and
never recomputed or cached in a handler, so the surface cannot disagree with the
resolver. Revocation is forward-only. A grant must state scope, limits and
expiry; no default expiry.

### `POST /approval-envelopes/preview` — would this act be waived

Body: `scope_kind` + `scope_id` (`goal` | `program` | `engagement`),
`capability`, `action`, `params` (default empty), `act_ref`, and optional
`engagement_id`, `engagement_identities`, `attachments_outside_ledger`,
`value_micros`. The effective request is derived from `params`, so a preview
cannot ask about a cleaner act than the one that would run.

`preview_approval_waiver` downgrades every live posture to shadow before
resolving: the answer is `advisory: true` and never a debit. `would_be_covered`
is true only when the reason is `not_enforcing` (a live envelope covers it and
only the posture stands between it and a waiver). With posture `off` the reply
says `ask` and names no envelope. `act_ref` is required (the debit is idempotent
on it); empty `capability` or `action` is refused (the consequence class derives
from the pair).

## Standing consent is spent, not just displayed

`approval_envelopes::waiver` sits behind `ApprovalGate::check`, the single
interpreter of `requires_approval`, so a prompt covered by a standing envelope is
dropped from the ask **and debited** in the same step; otherwise the envelope's
limits would be decorative. The consumption act key excludes the step id (it
carries the iteration number, so a resumed execution would double-debit) and the
model-authored goal text; parameters are ordered before hashing. With the
posture `off` (default) nothing is waived.

## `engagements_api` — who may act for whom

List (with live ceilings), detail, the cross-engagement read of one
counterparty's surface, the audience read (resolving the label against the
register), **create**, **renew** and **revoke**. Revocation is effective
**mid-run** (the engagement store enforces it at the dispatch boundary). Every
read and mutation is scoped to the authenticated principal.

Without an engagement there is no `EngagementAuthorityRef` on executions, so the
envelope scope is `None`, work bindings read `Unbound`, retrieval never confines,
and the maturity sweep finds no acts — all silently correct for a world with no
authority.

### `POST /engagements` — the write path

The body is **work-shaped**: a `work_context` (`{"kind": "program", "id": "..."}`)
rather than a `program_id`, so support triage, recruiting or vendor review bind
through the same route. `EngagementStore::grant_for_work` lowers it onto the
record, taking the ceiling from what the work says it needs.

Refusals, each fail-closed:

- **No expiry, no engagement.** `expires_at_ms` has no default; a past instant is
  refused.
- **An unenforceable ceiling is refused by name.** Each entry is checked against
  what this process can dispatch (registered providers, pack definitions and
  their action leaves, the executor's non-pack policy names). Matching is exact
  including case; a case-only miss is reported as `did_you_mean`, never
  substituted. Delegation names (`delegate_to_agent`, `handover_to_agent`) are
  refused separately — they are bounded by `team[]`.
- **No registry, no create** — the process could not tell a real ceiling from an
  inert one.
- **The counterparty label is checked with the register's own derivation.**

A retried create **resumes** the identical live engagement; changed terms are a
`409` naming what differs. Revoked and expired engagements are terminal.

### `PATCH /engagements/{engagement_id}` — renew, do not recreate

Moves the window forward on the same engagement. Re-creating would mint a new
ULID, and every execution, pause blob, envelope and disclosure naming the old id
would point at a lapsed authority. Forward-only; a revoked or clock-closed
engagement refuses. An identical renewal writes nothing and does not move the
revision.

## `counterparties_api` — who we know, and how we reach them

`POST /counterparties`, `GET /counterparties`, `GET /counterparties/resolve`,
`GET /counterparties/candidates`, `GET /counterparties/{counterparty_id}`,
`GET /counterparties/{counterparty_id}/audience`,
`POST /counterparties/{counterparty_id}/identities` and
`POST /counterparties/{counterparty_id}/identities/{identity_id}/promote`.
Mounted inside `/api/magician/v2`.

### The decider comes from the boundary, never from the body

`created_by`, `recorded_by` and `decided_by` come from the
`VerifiedRequestIdentity` the outer middleware attaches (not deserializable from
a payload). This is what makes the store's one real control work:
`promote_identity` refuses to let a `research_inferred` address be promoted by the
actor that recorded it, which is a string comparison a caller-chosen string
would defeat. An unproved request cannot write, and for a promotion
`request_authenticated` **is** the presence of that identity.

The promotion body names the **channel that carried the proof** (confirmed
click, delivery receipt, signed callback); `channel_is_verified` decides whether
it establishes anything. SMTP does not, and an unclassified channel fails closed.
`caller_claim` is `None` (a claim may only de-escalate).

### Resolution and affiliation are different routes

`GET /counterparties/resolve` returns `on_file` (the register holds this
address) and `proved` (an owner decided it reaches that organisation). **Only
`proved` may grant anything.**

`GET /counterparties/candidates` is domain affiliation, **for owner review
only**: the array is `review_candidates`, and an empty list means "nothing to
review" — never cleared, resolved or permitted.

`GET /counterparties/{id}/audience` takes `audience_kind` and works without any
engagement. It carries **verified addresses only**; an empty audience admits
nobody.

### Writes from the traffic paths

Two non-owner paths write; neither can verify anybody:

- **Inbound.** `/chat/active` and `/chat/new` call
  `chat::inbound_sender::identify_inbound_sender`, which resolves the sender and
  advances `last_seen` by one `observe` line (cannot touch verification,
  provenance or `first_seen`). It confers nothing: `authority()` is `Some` for
  exactly one state. The address kind comes from `channel_address_kind` or
  `kind_fixed_by_channel` (only where the transport fixes the shape, never for a
  handle).
- **Outbound.** `ChatService::relay_envoy_owner_decision` (from
  `POST /hitl/{correlation_id}/respond`) records the address an owner-approved
  reply is going to — **always unverified** — before the relay spawns. It never
  invents an organisation; an unknown address files nothing and returns as a
  lead.

## `work_modules_api` — the four composable work modules

`magician-api/src/work_modules_api.rs`, mounted under `/work`: `run_state`,
`scheduling`, `claim_manifest` and `obligations`, plus
`GET /work/introducers`, `GET /work/negotiations/open-ask`, and
`POST /work/runs/inbox-sweep`.

One file, not four: handlers resolve a scope, guard caller strings that feed a
derived id, and delegate, so scope resolution, the separator guard, the audience
parser and the unreadable-store refusal exist once. The modules' seams stay
apart: a run cannot reach into a negotiation, nor the register back into either.

### Durable runs — `/work/runs`

`GET|POST /work/runs`, `GET /work/runs/{run_id}`, `POST /work/runs/inbox-sweep`,
and writes `/fields`, `/answers`, `/gaps`, `/gaps/resolve`, `/expectations`,
`/expectations/fulfil`, `/submit`.

- `GET /work/runs/{run_id}` returns the run, **what it waits for**
  (`expectations_awaiting`) and **what needs a human** (`gaps_for_owner`). Each
  wait's age is derived from the clock on read, never stored.
- `/gaps/resolve` puts the reply's ref in `evidence_refs` (owner facts clear the
  same grounding bar) and requires `resolved_by`.
- `/expectations/fulfil` takes an already-extracted event (reads no inbox) and
  answers `fulfilled`, `already` or `unmatched` — not `404`, since an inbox
  carries far more than one run's verification.
- `/submit` records the covering outward act on the `Form` channel **first**,
  then seals the run; `submitted` requires a named person and a payload artifact
  ref.
- Listing uses `RunStateStore::all_runs`, since run ids are derived from
  `(scope, purpose, resource_ref)` and nothing else indexes them.
- `POST /work/runs/inbox-sweep` (body: optional `workspace`) reads the mailbox
  configured as `run_inbox` (`magician_config.run_inbox`). Without it the route
  answers **503** `run_inbox_no_source`, not an empty sweep. Nothing is consumed
  or moved; the run store's idempotency on the event ref is the cursor. The reply
  carries denominators (`examined`, `accounted_for`, `runs_waiting`) and outcomes
  `fulfilled`, `already_fulfilled`, `unmatched`, `ambiguous`, `refused`. A source
  more than one run waits on closes **neither**; one misconfigured run does not
  block others.

### Time negotiations — `/work/negotiations`

`GET|POST /work/negotiations`, `GET /work/negotiations/{id}`,
`GET /work/negotiations/open-ask`, and `/reply`, `/re-offer`, `/hold`,
`/reschedule`, `/close`.

- **No channel is named anywhere.** `offer_message` returns words *and* times;
  the caller sends on whatever capability the conversation uses and returns the
  act ref on the next write. `hold_intent` returns what a calendar needs and
  books nothing.
- Both intents return either the intent or a `refusal` in the module's words,
  never an omission ("nothing to send" differs from "must not send").
- `/hold` reads the slot off the acceptance, never the request.
- `/re-offer` exists because `open` cannot record a changed offer; without it an
  acceptance of new times would be refused.
- `GET /work/negotiations/open-ask` looks up by `audience_kind` + `audience_id`,
  or `channel` + `channel_address` (optional `channel_address_kind`,
  `channel_verified`, which may only de-escalate; same two proofs as
  `/chat/active`). Answers: `one_open_ask` (absorb against `negotiation_id`),
  `no_open_ask` (not an error), `ambiguous_open_asks` (caller chooses). It reads
  no reply and absorbs nothing. Registered before
  `/work/negotiations/{negotiation_id}`; derived ids (`neg-<hash>`) can never be
  `open-ask`.
- `SchedulingStore::all_negotiations` backs the unfiltered listing and sweep
  (logs are named by audience-key hash).

### Claim manifests — `/work/claim-manifests`

`POST` binds a revision's claim set; `GET ?artifact_ref=[&revision_ref=]` reads an
artifact's claim history; `GET /carrying?claim_ref=[&latest_only=]` is the
**correction-propagation query** (decks and drafts still carrying a corrected
claim, which the sent-record cannot find). A revision's claims are immutable: an
identical (order-insensitive) rebind resumes, a different one is `409`; the fix
is a new revision.

### The register — `/work/obligations`

`GET` answers what is owed and what lapsed across relationships, counting the two
directions apart. `POST` records a promise by hand; `POST /{id}/settle` closes one
as `met` or `released` (a release requires a reason, so the hit rate is not
partly wishful). `POST /work/obligations/sweep` runs the data-room follow-up and
scheduling silence sweeps now — the on-demand twin of
`obligation_sweeps::worker`. See `docs/components/magician/obligations.md`.

`GET` also carries what the counterparty did: `obligation_sweeps::attention`
composes a reading and `explain_register` joins it by `obligation_id` (row gains
`reading`, response gains `readings`). It is this route rather than a second
queue, so there is one source of truth for what is outstanding. The join is
fail-closed: unknown handles go under `readings.row_not_in_register`; an empty
register attaches nothing; readings that raised nothing go under
`readings.not_yet_raised`; `readings.available` is `false` (never zeroed counts)
when rooms cannot be read, while the register is still answered. Readings are
derived on demand and never recorded (they carry a visit count, which must not
enter the identity tuple), under `WorkModulesApi`'s `ObligationSweepConfig` —
the worker's own config type, set with `with_sweep_config` — because `due_at` is
built from those windows and is part of the derived id. The response states the
windows used.

### Introducer debts — `/work/introducers`

Who introduced us to whom, and which introducers we owe an update. It
**proposes; it does not record** — agreement goes through `POST /work/obligations`.
Query: optional `workspace`, `update_after_days` (default 30, refused outside
1–3650). Reply denominators: `introductions` and `introducers`; `owed` lists
introducers past the window with a `proposed_obligation`. An unreadable register
fails the request rather than returning a partial graph.

### Refusals that shape every handler

- **`U+001F` is refused at the door** (`guard_id_component`, before any store).
  The scheduling store, claim manifest store and obligation register join caller
  strings with that separator to derive ids, so a crafted component could fuse
  two identities across tenants.
- **An unreadable store is `500`, never `[]`.**
- **A missing run, negotiation or obligation is `404`**, never an empty list.
- **A store's refusal is `409` in its own words** (not ready to submit, slot never
  offered), because the caller can act on it.
- **No route derives a time state.** Lapsed, silent, awaiting and ready are
  derived by the modules from the instant the handler passes in.

## `suppression_api` — and the delivery routes beside it

`magician-api/src/suppression_api.rs`, mounted by `configure_suppression_routes`
(called once from `magician-bin/src/main.rs`). Register acts:
`GET|POST /suppressions`, `GET /suppressions/check`, `POST /suppressions/lift`.
Delivery routes in the same file (one subject: what became of what we sent,
from the same ledger `magician_v2::delivery_hygiene` reads):

| route | question |
|---|---|
| `GET /delivery/unacknowledged` | what did we send that nothing ever came back for |
| `GET /delivery/watch` | is anything asking that question on a cadence |
| `POST /delivery/reindex-work-axes` | re-file existing acts under the work they already name |
| `POST /delivery/receipts` | **record what a provider said about one act** |
| `GET /delivery/receipts` | what is on file for one act, and who put it there |

The mail door (`POST /delivery/sent`, `POST /delivery/receipts/mail`) is
`delivery_receipts_api`, mounted beside these by `configure_delivery_mail_routes`.

### `GET /delivery/unacknowledged`

Answers `DeliveryLedger::unreconciled`, the signal for a silently broken provider
integration (every send succeeds, no receipt arrives). Response: counts with
denominator, overdue acts oldest first, and per-rail counts so the moving line
names the broken integration. `grace_hours` defaults to 24 and is always echoed.
Everything derives from the clock on read; the read records nothing.

Fail-closed readings that would otherwise be a green zero:

- `any_dispatch_recorded: false` plus an explicit `quiet` field distinguish "never
  dispatched" from "everything confirmed".
- An unreadable dispatch log answers **503**, never 200 with an empty report.
- An act whose rail cannot be named counts under `unattributed`, never inside a
  named rail.

The rail is the act's own `OutwardChannel`, read from the owning disclosure (not
copied onto the dispatch row). Attribution is a **supplied map**, so a rail
recorded elsewhere supplies its own without editing a handler.

### `GET /delivery/watch`

The health snapshot of `delivery_hygiene::worker::SuppressionSweepWorker`, whose
tick also reads silence for every configured scope. When the binary did not
attach it, the route answers **503** `delivery_watch_not_wired`: "no watcher is
running" must never render as "the watcher found nothing". `state` says whether
suppressions are being recorded; `silence_state` says whether anything comes
back — `degraded` when overdue acts reach the floor (default 1, naming the worst
rail), `no_dispatches` (never `idle`) when no scope has dispatched.

### `POST /delivery/reindex-work-axes`

Re-files every outward act in a scope under the work axes its own record names,
so programme/engagement history is reachable to the sweeps (body: optional
`workspace`). It **re-indexes, never reconstructs**: an act whose record names
no work stays `unattributed` (guessing from recipient or nearest engagement would
file acts under a relationship nobody chose). Read `unattributed` and
`unattributed_is_not_repairable`, not the 200. The reply also carries
`acts_seen`, `engagement_entries`, `program_entries`, `already_indexed`.

## `POST /delivery/receipts` — the door a receipt comes through

The production caller of `DeliveryLedger::reconcile`; without it every live send
stays `dispatch_unknown`. The handler holds no store logic: it proves the caller,
resolves scope from that proof, and passes this scope's dispatch log and the
receipt to `delivery::intake::ReceiptIntake::admit`, which hands it to
`reconcile` untouched. The response names the disposition (`opened` /
`replayed` / `advanced` / `superseded`), what held when order refused a receipt,
identity and act state, and the `suppression_cause` the hygiene sweep will act
on.

**One door, any provider.** Provider bridges, a Kapso adapter, an SMTP reconciler
and an owner transcribing a bounce POST the same body. `source` (`provider` or
`operator`, required) records the route the fact travelled and changes nothing
about how it is judged; the operator path adds a `payload_ref` to the
out-of-band report.

### Authentication is the whole risk

An open receipt endpoint is a **remote suppression primitive**: `complained` and
hard bounces become register entries only an owner act with evidence can lift,
and `delivered` silences a real failure. The Magician API has no
provider-signature middleware for an unattended inbound POST (every
`/api/magician/v2` route is behind `verify_access_middleware`, whose only bypass
is `POST /devices/enrollment/exchange`), and the Node webhook receivers carry no
delivery stream. So both receipt routes are **owner-authenticated only**:

- require `VerifiedRequestIdentity` (Cloudflare Access, paired device, or real
  loopback peer — the `apps_api` gate);
- scope comes **from that identity**, never `X-Principal` / `X-Workspace`; a
  disagreeing header is **403**;
- a workspace-bound identity (paired device, loopback) uses its own; an
  interactive Access owner must name one (absent is 400);
- the **read** is gated like the write, because it names who recorded each
  receipt.

Consequences: the loopback identity is bound to `anonymous`/`default` (inherited
from `trusted_loopback_request_identity`), so an unattended local bridge can only
reconcile that scope. A provider-direct webhook would need a per-provider signing
key, a path exemption in the access middleware, raw-body capture before JSON
parsing, and a replay window keyed on the provider's event id.

### Refusals

- **An act that never left.** `admit` refuses a receipt whose act is not on this
  scope's dispatch log; a scope that has dispatched nothing is a separately named
  refusal (wrong scope or unwritten log).
- **An unnamed state or source.** No defaults: `accepted` would make a broken
  integration look acknowledged; `provider` would misfile a person's
  transcription. `state` is read off `DeliveryState::ALL`.
- The body is `deny_unknown_fields` with no principal, workspace or actor field.

### `GET /delivery/receipts` — including refused attempts

Observations plus every attempt to add one. The attempt log is written **before**
reconcile, so a refused attempt still names who made it. It carries no
disposition (the outcome lives only in the ledger). `dispatch_recorded: false` is
reported, not refused — the missing act is itself the finding.

## `delivery_receipts_api` — the mail door

`magician-api/src/delivery_receipts_api.rs`, mounted by
`configure_delivery_mail_routes` beside the suppression routes.

| route | question |
|---|---|
| `POST /delivery/sent` | register what left, under the identifier a bounce will quote back |
| `POST /delivery/receipts/mail` | hand in one raw RFC 5322 message; correlated DSN recipients become receipts |

No provider wired into this deployment emits an email delivery event stream; a
hard bounce arrives as an RFC 3464 DSN. `provider` is a parameter, so an inbox
reader, an SMTP bounce mailbox and an owner pasting a forwarded bounce use the
same calls, and each correlated receipt goes through the same
`ReceiptIntake::admit`. Caller-proving restates `suppression_api::receipt_caller`
(scope from identity, disagreeing header **403**, unproved **401**). App state is
the `SuppressionApi` workspace handle.

### `POST /delivery/sent` — how a report will name this send

The dispatch log records *that* an act left; this records *how a report will name
it*, possibly later (a rail may learn its message id only from the provider). It
does **not** require the act to be on the dispatch log (reported as
`dispatch_recorded`; `admit` still refuses receipts for undispatched acts). Body
(`deny_unknown_fields`): `act_ref`, `provider`, optional `message_id` (RFC 5322,
with or without brackets), optional `envelope_id` (RFC 3461) — at least one
required — `audience` (non-empty; every bounce address is checked against it), and
`sent_at`. Identical registration is one row; a different act, audience or
instant under one identifier is refused.

### `POST /delivery/receipts/mail` — one message, through the same intake

Body (`deny_unknown_fields`): `provider`, `payload_ref`, `raw_mail` (raw RFC 5322
message, ≤ 1 MiB), `source` (`provider` | `operator`, required), `dry_run`
(default false, returns `would_record`). Ordinary mail answers **200**
`recognised: false`; an unreadable self-declared delivery report **422**; a store
fault **503**. A bounce that cannot be tied to a send is recorded nowhere and
listed under `refused` (guessing would permanently suppress the wrong person). A
duplicate message returns `replayed`; changing `payload_ref` between posts of the
same message is an error.

## Why HTTP readers pass `RetrievalScope::Unbound`

Engagement-scoped retrieval confines what an **execution** carrying an engagement
authority may see (unlabelled context is refused). HTTP handlers that read memory
for the signed-in owner — analytics memory views, the VibeDev rail — pass
`RetrievalScope::Unbound` explicitly with a comment saying why: **the owner
inspecting their own memory is the one reader an engagement boundary is not
drawn against.** The comments distinguish these deliberate uses from an
unconfigured call site that would get `Unbound` silently.
