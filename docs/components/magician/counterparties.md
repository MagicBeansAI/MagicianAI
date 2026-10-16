# Counterparties

Module: `magician_v2::counterparties`, a facade over `counterparty_store.rs`,
`counterparty_consumers.rs` and `counterparty_types.rs` in
`magician/src/magician_v2/`.
Plan: `docs/archive/plans/2026-08-07-opc-engagements-contextual-authority.md` §3.

**Who we are actually talking to**: an external organisation and the identities
through which we reach it. It is as much a CRM, support or recruiting primitive
as a deal one; nothing in its public API names fundraising.

## Provenance is the point

Every identity records **how we learned it** (`OwnerStated`,
`ObservedOnInbound`, `ResearchInferred` or `Introduced`), the evidence that
established it, and the identity that vouched (`introduced_by`). A researched
address is a lead, not a contact: `ResearchInferred` is unverified by
construction, and `promote_identity` refuses to promote one without an explicit
owner decision with separate evidence. It also refuses when the decider is the
party that minted the identity, so a researcher cannot approve their own
inference.

Verification comes only from a server-trusted signal, delegated to
`chat::envoy::channel_is_verified`; an unclassified channel or a caller-claimed
truth fails closed.

## Resolution is exact, and that is a safety property

`resolve(scope, kind, value)` matches the **normalised value exactly**. It never
guesses, matches fuzzily or infers from a shared domain, because a wrong answer
hands one counterparty's authority to another. Domain matching exists only as
`candidates_by_domain`, which returns candidates **for owner review**, never an
automatic resolution. `resolve_verified` is the authority-bearing variant; plain
`resolve` establishes affiliation, not permission.

`identity_id` derives from `(principal, workspace, kind, normalised)` and
deliberately **not** from the counterparty, so binding one address to a second
organisation derives the same id and is refused instead of creating an
ambiguous row.

## Shape

One append-only JSONL log per scope, folded on read, through `magician_v2::jsonl`
only. Ids are blake3 over U+001F-joined components, so a retry resumes. Reads
fail closed: an unreadable log is an error, and `identities_for` on an unknown
counterparty is an error, not an empty "no addresses" answer.

`audience_for` exposes **only verified** identities, so an unproved counterparty
yields an empty audience that admits nobody, even addresses held unverified.

A merge is an appended Merged-into edge the fold follows, never a rewrite. Merge
cycles are refused, a malformed chain terminates instead of looping, and merged
records are terminal: they cannot be re-recorded, re-merged or filed under.

## Where it is used

**Two reader paths ask only the verification question.**

`data_room_reader_api::CounterpartyAudiences` wraps a `CounterpartyStore` and
answers "who may be admitted to this room" with `audience_for`. It is built in
`magician-bin/src/main.rs` and serves `GET /rooms/{room_id}` and
`GET /rooms/{room_id}/document`.

`engagements_api` serves `GET /engagements/{engagement_id}/audience`, "who may
this engagement reach". An engagement carries an owner-typed counterparty
**label**, not an id, so the route hands it to `consumers::audience_for_label`;
the resolution lives here, and the identity machinery never learns what an
engagement is. It reads the register through
`counterparties::global_counterparty_store`; until `magician-bin` installs one
the route answers 503, because unreadable is never `nobody`.

**The owner's own surface writes it.** `magician_api::counterparties_api` serves
`POST /counterparties`, `POST /counterparties/{id}/identities` and
`POST /counterparties/{id}/identities/{identity_id}/promote`, plus list,
resolve, detail, audience and domain-candidate reads. `created_by`,
`recorded_by` and `decided_by` come from the boundary's
`VerifiedRequestIdentity` and **cannot** be sent in a body: the store's "the
guesser may not approve its own output" check compares recorder and decider, so
a caller choosing either string would defeat it. Every write body is
`deny_unknown_fields`, so trying is a `400`, not a silently dropped field.

**The inbound path identifies, and writes exactly one line.**
`chat::inbound_sender::identify_inbound_sender` (through
`identify_inbound_sender_in_process`) is called from
`GET /api/magician/v2/chat/active`, `POST /api/magician/v2/chat/new` and
`GET /work/negotiations/open-ask` (`work_modules_api::resolve_reply_audience`). It
resolves the sender through `resolve_inbound`, which advances `last_seen`
through `observe` (the one write that cannot touch verification, provenance or
`first_seen`), and returns an `InboundSender` whose `authority()` is `Some` for
exactly one state. The address **kind** comes from the caller
(`channel_address_kind`) or else from `kind_fixed_by_channel`, which answers only
for channels whose transport *is* the address format, never for a handle. If
neither answers, nobody is identified and nothing is written.

**The outbound path records where an owner-approved reply goes.**
`ChatService::relay_envoy_owner_decision`, the envoy diode's return leg reached
from `POST /api/magician/v2/hitl/{correlation_id}/respond`, calls
`counterparties::outbound::record_outbound_write` before the relay is spawned. It
always files the address **unverified** and never invents an organisation: the
diode carries no organisation label, so `OutboundOrganisation::FromRegister` is
the only honest source, and an address the register does not hold files
nothing.

`chat::inbound_authority::engagement_lane_for_inbound` turns an
*authoritative* inbound identification into an engagement lane; the chat API
reaches it through `engagement_lane_in_process` and passes the lane to
`envoy::resolve_inbound_lane`. `merge` has no production caller yet.

## Resolving a work label

`resolve_label(scope, label)` answers `LabelStanding`: either the surviving
`CounterpartyRef` or a `CounterpartyLead`. A lead is **not an error**; a flow may
name an organisation nobody has filed yet. It carries the id
`record_counterparty` would derive for that label, so the owner's follow-up lands
on the same row. Reading a lead **writes nothing**: growing the register from a
form field would make a typo a permanent organisation with no decider.

A lead reads fail-closed as an **empty identity set**:
`verified_identities_for_label` answers `[]` and `audience_for_label` an
`Audience` with no members. The label naming nobody, the organisation having no
addresses, and none being proved all yield that same empty answer, and all mean
the same to a caller. A predicate that passes vacuously over the empty set is the
bug this shape rules out.

`audience_for_label` takes the `AudienceKind` from the caller, so support
triage, vendor management and recruiting reuse the path with a different kind.
The kind changes the audience **key**, never the membership.

## Not built here

Engagements still carry a string label, not a `CounterpartyRef`; `resolve_label`
lets them adopt real identities later without a migration. Today the label is
authoritative and the reference is a lookup result.
