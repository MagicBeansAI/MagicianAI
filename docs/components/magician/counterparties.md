# Counterparties

Module: `magician/src/magician_v2/counterparties/`.
Plan: `docs/archive/plans/2026-08-07-opc-engagements-contextual-authority.md` §3.

**Who we are actually talking to** — an external organisation, and the set of
identities through which we reach it. Generic: this is a CRM, support or
recruiting primitive as much as a deal one. Nothing in its public API names
fundraising.

## Provenance is the point

Every identity records **how we learned it** — `OwnerStated`, `ObservedOnInbound`,
`ResearchInferred` or `Introduced` — with the evidence that established it, plus
the identity that vouched (`introduced_by`). A researched address is a lead, not a
contact: `ResearchInferred` is unverified by construction and `promote_identity`
refuses to promote one without an explicit owner decision carrying separate
evidence. It also refuses when the deciding party is the same party that minted
the identity, so a researcher cannot approve their own inference.

Verification comes only from a server-trusted signal, delegated to
`chat::envoy::channel_is_verified` — an unclassified channel and a caller-claimed
truth both fail closed.

## Resolution is exact, and that is a safety property

`resolve(scope, kind, value)` matches the **normalised value exactly**. It never
guesses, never matches fuzzily, and never infers from a shared domain, because a
wrong answer hands one counterparty's authority to another. Domain matching
exists only as `candidates_by_domain`, which returns candidates **for owner
review** and is never an automatic resolution. `resolve_verified` is the
authority-bearing variant; plain `resolve` establishes affiliation, not
permission.

`identity_id` derives from `(principal, workspace, kind, normalised)` and
deliberately **not** from the counterparty — so binding one address to a second
organisation derives the same id and is refused, rather than creating an
ambiguous second row.

## Shape

One append-only JSONL log per scope, folded on read, through
`magician_v2::jsonl` only. Ids are blake3 over U+001F-joined components, so a
retry resumes. Reads fail closed: an unreadable log is an error, and
`identities_for` on an unknown counterparty is an error rather than an empty
answer that reads as "no addresses".

`audience_for` exposes **only verified** identities, so an unproved counterparty
yields an audience with zero members that admits nobody — including the
addresses the register holds unverified.

A merge is an appended Merged-into edge the fold follows, never a rewrite. Merge
cycles are refused and a malformed chain terminates rather than looping. Merged
records are terminal: they cannot be re-recorded, re-merged, or filed under.

## Where it is used

**Two reader paths ask only the verification question.**

`data_room_reader_api::CounterpartyAudiences` wraps a `CounterpartyStore` and
answers *"who may be admitted to this room"* with `audience_for` — verified
identities only, so an unproved counterparty yields an audience that admits
nobody. It is constructed in `magician-bin/src/main.rs` and serves every
`GET /rooms/{room_id}` request.

`engagements_api` serves `GET /engagements/{engagement_id}/audience`, which
answers *"who may this engagement reach"*. An engagement carries an owner-typed
counterparty **label**, not an id, so the route hands the label to
`consumers::audience_for_label` — the resolution lives in this module, so the
identity machinery never learns what an engagement is. It reads the register
through `counterparties::global_counterparty_store`; until `magician-bin`
installs one the route answers 503, because unreadable is never `nobody`.

**The owner's own surface writes it.** `magician_api::counterparties_api` serves
`POST /counterparties`, `POST /counterparties/{id}/identities` and
`POST /counterparties/{id}/identities/{identity_id}/promote`, plus the list,
resolve, detail, audience and domain-candidate reads. `created_by`,
`recorded_by` and `decided_by` are taken from the boundary's
`VerifiedRequestIdentity` and **cannot** be sent in a body — the store's
"the guesser may not approve its own output" check compares the recorder against
the decider, so a caller that chose either string would defeat it while looking
like it enforced it. Every write body is `deny_unknown_fields`, so trying is a
`400` rather than a silently dropped field.

**The inbound path identifies, and writes exactly one line.**
`chat::inbound_sender::identify_inbound_sender` is called from
`GET /api/magician/v2/chat/active` and `POST /api/magician/v2/chat/new`. It
resolves the sender through `resolve_inbound` — which advances `last_seen`
through `observe`, the one write that cannot touch verification, provenance or
`first_seen` — and returns an `InboundSender` whose `authority()` answers `Some`
for exactly one state. The address **kind** comes from the caller
(`channel_address_kind`) or, failing that, from `kind_fixed_by_channel`, which
answers only for channels whose transport *is* the address format and never for
a handle. Neither answering identifies nobody and writes nothing.

**The outbound path records where an owner-approved reply goes.**
`ChatService::relay_envoy_owner_decision` — the envoy diode's return leg,
reached from `POST /api/magician/v2/hitl/{correlation_id}/respond` — calls
`counterparties::outbound::record_outbound_write` before the relay is spawned.
It files the address **unverified**, always, and never invents an organisation:
the diode carries no organisation label of its own, so
`OutboundOrganisation::FromRegister` is the only honest source and an address the
register does not hold files nothing.

Still uncalled: `chat::inbound_authority::engagement_lane_for_inbound` — the
join that turns an *authoritative* inbound identification into an engagement
lane — and `merge`. The chat ingress produces the identification the join needs;
nothing yet feeds it to `envoy::resolve_inbound_lane`.

## Resolving a work label

`resolve_label(scope, label)` answers `LabelStanding` — either the surviving
`CounterpartyRef` or a `CounterpartyLead`. A lead is **not an error**: a flow may
legitimately name an organisation nobody has filed yet. It carries the id
`record_counterparty` would derive for that label, so the owner-facing follow-up
lands on the row the read was already answering under. Reading a lead **writes
nothing** — growing the register from a form field would make a typo a permanent
organisation with no decider.

The fail-closed reading of a lead is an **empty identity set**:
`verified_identities_for_label` answers `[]` and `audience_for_label` answers an
`Audience` with no members, which admits nobody. Three different facts produce
that empty answer — the label names nobody, the organisation has no addresses,
or none of its addresses is proved — and all three mean the same thing to a
caller. A predicate that passes vacuously over the empty set is the bug this
shape exists to make impossible.

`audience_for_label` takes the `AudienceKind` from the caller, so support
triage, vendor management and recruiting adopt the same path by passing a
different kind. The kind changes the audience **key**, never the membership.

## Not built here

Engagements still carry their string label rather than a `CounterpartyRef`;
`resolve_label` is what lets them adopt real identities later without a
migration. Today the label is authoritative and the reference is a lookup
result.
