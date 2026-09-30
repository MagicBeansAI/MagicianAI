# The outward gate

Module: `magician/src/magician_v2/agents/outward_gate.rs`, called from the outward
branch of `execute_action_inner` — the one function every action passes through.

Five refusals stand between an agent deciding to act outward and the act
leaving. They run in a fixed order, and the order is the design:

```
disclosure record        the act is written down before anything else can stop it
0. work context          does the work this execution is bound to narrow to this capability
1. restriction           is the act reducible to a bindable form at all
2. suppression           may we contact this person
3. envelope shadow       would standing consent have covered it
4. capture / dispatch     record-only, or actually send
```

**Suppression comes before consent.** Consent to run a programme is not consent
to contact somebody who opted out of it, so an envelope can never authorise
reaching a suppressed identity.

**Work context comes first** because it decides nothing about who the act
reaches — an actor reaching for a capability its work does not narrow to is
refused before anyone inspects the recipients.

**Gates 0 through 2 run under capture as well as live.** They sit above the
capture/live branch, so capture never rehearses an act that could never be
authorised: a passthrough send, a suppressed recipient and a capability outside
the work are all refused under today's default posture, and the refusal is
written onto the disclosure as `failed`. Only the envelope shadow and the
dispatch itself sit below the branch.

**Gate 0 is live when an execution carries work.** `work_binding_for_dispatch`
reads `ctx.work_authority`. `POST /api/magician/v2/engagements` mints the roster
row through `grant_for_work` → `EngagementStore::create`. A root execution binds
to a live engagement via `root_authority_for_work`, and the durable
`work_authority` is copied onto the context at resume. An engagement carrier is
resolved against the store's live ceiling; a missing, expired or revoked
authority is `Unresolvable` (a refusal). An execution that carries no work is
still `WorkBinding::Unbound`, which declines to answer rather than passing.
Program carriers can be enforced at dispatch, but `root_authority_for_work`
still refuses to mint one: no roster owns programs.

## Every refusal is fail-closed

- **No recipient, on a channel that requires one.** "We could not tell who this
  reaches" is not permission to reach them.
- **No scoped store.** An unconsultable suppression register is not an empty one.
- **An unreadable register.** `is_suppressed` returning `Err` means *do not send*.
- **Nobody cleared.** The verdict is read off the *sendable* list, never off
  `blocked.is_empty()`, so a screen that blocks nobody and clears nobody refuses.

Recipients come from `resolve_effective_action`'s list — the same answer the
disclosure record and the envelope shadow use — not from the typed `to` field.
Reading `to` would screen one address for an act that reaches five, and the four
the screen never saw are exactly the ones a passthrough added.

## Reaching nobody is not the same as being unreadable

`Addressing` splits the two meanings of an empty recipient list, because the
fail-closed reading is correct for only one of them. Mail and messages exist to
reach a named person, so empty means we failed to parse and the act refuses. A
calendar entry with no attendees is an entry on the owner's own calendar — it
reaches nobody, there is nobody to screen, and refusing it would block the act
`scheduling::consumer` performs whenever a slot is agreed. The distinction is
derived from the capability's own `OutwardClass`, **never from the arguments** —
the arguments are what we just failed to read. The act is recorded as a
disclosure either way; only the screen is skipped.

## Dispatch is recorded, so silence is visible

A dispatched act is written to an append-only dispatch log before the disclosure
is marked dispatching, and all three records share one clock reading — a
disagreement between them would be exactly the size of the silence window a
follow-up sweep measures. `DispatchLog::unreconciled` pairs that log against
`delivery::DeliveryLedger`, and deliberately propagates the ledger's
empty-candidate refusal rather than returning an empty clean bill of health.

**The silence sweep.** `delivery_hygiene::silence` reads that pairing from two
places: the delivery-hygiene worker's tick, and
`GET /api/magician/v2/delivery/unacknowledged`. No adapter or webhook reports
provider receipts automatically; the operator door exists —
`POST /api/magician/v2/delivery/receipts` admits a receipt through
`ReceiptIntake::admit`, which calls `DeliveryLedger::reconcile`. Until a
provider pushes one unprompted, silence remains a reported number. See
`docs/components/magician/delivery.md`.

Refusals mark the disclosure `failed`, so a refused act reads as refused rather
than sitting at `prepared` forever — and a failed disclosure is not active, so a
refused act correctly owes no corrections.
