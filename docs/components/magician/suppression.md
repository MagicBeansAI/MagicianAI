# Suppression

Module: `magician/src/magician_v2/suppression/`.
Named as a hard gate for live sending in
`docs/archive/plans/2026-08-07-opc-readiness-review.md` §9B.

**Identities we must not contact, and why.**

## Reads fail closed, and that is the whole point

`is_suppressed(scope, identity, now)` returns an **error** when the register
cannot be read, and the calling contract is that an error means *do not send*.
An unreadable register must never read as "nobody is suppressed". Identities
are normalised (case and whitespace) on both write and read.

## A lift is an act, not a deletion

Suppression is append-only. A lift is a separate recorded event carrying who
decided it and on what evidence. `OptOut` and `Complaint` cannot be lifted by
anything except an explicit owner act with evidence.

Reasons: `OptOut`, `HardBounce`, `Complaint`, `OwnerBlocked`, `RegulatoryHold`.
Each carries when it was established and the ref that established it.

## Global by default

Suppression is **global to the owner**, not per-audience: someone who opted out
of one programme has not consented to another. `SuppressionRegister::global` is
the safe constructor. `scoped_to_audience` exists for a named cohort; that
register still consults the global log on every read and cannot lift a global
entry.

## Where it is enforced

**Gate 2 of the outward gate.** `outward_gate::contact_refusal` constructs
`SuppressionRegister::global` and `screen`s the recipient list
`resolve_effective_action` produced — the same list the disclosure record and
the envelope shadow use, never the typed `to` field. It runs inside
`execute_action_inner` ahead of the capture/live branch, so a message to a
suppressed identity is refused under capture as well as live. The verdict is
read off `sendable`, never off `blocked.is_empty()`.

## Delivery join

`delivery_hygiene` is the join. `sweep_delivery_into_suppression` reads
`DeliveryLedger::suppression_signals` via `signals_since`, maps
`HardBounce`/`Complaint` through `reason_for`, and calls `register.ingest`.
Evidence is `SuppressionEvidence::new(since,
"delivery:{principal}:{workspace}:since:{ts}", "delivery-hygiene-sweep")`.
A sweep that names one owner's ledger and another owner's register is refused.

`magician-bin` starts `SuppressionSweepWorker::spawn_with_receipts` on the
`delivery_hygiene` config cadence (`enabled` defaults on; an enabled sweep
with an empty scope list refuses to start). The cursor (`HygieneCursor`) is a
bookmark: an absent cursor means the beginning of time, never `now`; losing it
costs work, never truth, because ingest is idempotent per
`(identity, reason, evidence)`. The worker never lifts. Owner APIs
`POST /suppressions` and `POST /suppressions/lift` remain the other writers.

The same tick can watch silence and pull bounce receipts when a mailbox is
configured; those are sibling questions, not a second suppression mapping.
