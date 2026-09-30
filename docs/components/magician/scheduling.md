# Scheduling — negotiating a time with someone outside

Composable Work Modules, **Module A**. Module: `magician/src/magician_v2/scheduling/`.

*"Propose, agree and hold a time with someone outside the organisation."* The
plan's two load-bearing notes shape everything:

- *"The invite returns on a channel… it must resolve to the same counterparty."*
  Identity resolution is the **caller's** job via the audience; this module
  takes an `AudienceRef` + counterparty identity and never parses an email.
- *"Silence is a state."* Derived, never stored — `silent_since(window, now)` —
  and it ripens into the obligations register (`OwedToUs`: they owe an answer),
  with a sweep-stable due date so repeated sweeps collapse to one obligation.

**Reachable from `work_modules_api` and the obligation sweep.**
`magician-api/src/work_modules_api.rs` mounts
`/api/magician/v2/work/negotiations` — open, absorb a reply, re-offer, hold,
reschedule, close, and a listing across every relationship. The offer and the
hold come back as **intents**: `offer_message` returns words and times and
`hold_intent` returns what a calendar needs, and neither names a channel, so the
send and the booking stay the caller's on whatever capability the conversation
is on.

Silence lands in the obligations register.
`magician_v2::obligation_sweeps` runs `sweep_silence` over every negotiation in
a scope and writes the result through `apply_silence_sweep` — record everything,
then settle everything — reachable from
`POST /api/magician/v2/work/obligations/sweep` and from
`obligation_sweeps::worker::ObligationSweepWorker`.

Two reads make that possible. `all_negotiations` folds the scope's whole
directory, because logs are named for a hash of the audience key and a sweep
asking *"which asks has nobody answered"* must not be limited to relationships
somebody already named. `apply_silence_sweep` is the write half:
`silence_obligations` pairs every chase with the id it lives under, and
`apply_silence_sweep` records and settles them.

## What it deliberately does not do

It records the negotiation. It never reads a calendar, never sends an offer,
never books an event — those are outward acts the consumer performs, and the
record links to them (`offer_act_ref`, `calendar_event_ref`) so the outward
assertions trail and the negotiation agree about what happened.

## The state machine, derived

Offered slots stand until a reply replaces them: a counter **replaces** the
standing slots, so accepting a slot nobody currently offers — including the
original offer after a counter — is refused as *recording an agreement that
never happened*. `hold` requires the accepted slot exactly; `reschedule` is only
from Held, clears the dead times, and keeps full history. Reopening a **closed**
round starts a fresh round in the same slot (the id stays derived from
audience + counterparty + purpose) with history intact — a closed round is not
a permanent tombstone for a recurring counterpart.

A genuinely new offer after a decline or counter goes through `re_offer`, which
replaces the standing slots; `open` resumes only an identical replay and
refuses anything else by name.

## Failure semantics

Absent-vs-unreadable and torn-tail rules come from `magician_v2::jsonl`. Absorb
is idempotent on `source_ref` — a re-read inbox must not double-record a reply.
Mutations are unlocked read-validate-append (no compare-and-append exists on
append-only files); replays converge via first-wins folds, documented per
method.
