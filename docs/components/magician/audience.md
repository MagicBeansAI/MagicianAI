# Audience — who may see something

Module: `magician/src/magician_v2/audience/`.

A **named, enumerable set of identities with a lifetime.**

## Why it exists

A data room is a bounded set of documents shared with a bounded set of people,
whose access derives from a relationship and ends with it. That describes a deal,
cohort materials, a client's deliverables, an audit pack and somebody's own
records equally well, so access binds to an **audience** rather than only to an
**engagement**.

## The kinds

| kind | example |
|---|---|
| `Engagement` | a live deal with a counterparty — the original case |
| `Program` | a cohort, an intake, a launch |
| `Account` | a client or supplier, across many engagements |
| `Panel` | auditors, a board, a review committee |
| `Person` | one individual — their own records, their own offer |

Every variant names something **whose members you could list**. Adding one that
does not would break the invariant below.

## The invariant, enforced by the type

**An audience is always enumerable.** If you cannot list who is in it, it is not
an audience — it is *publication*, a different consequence class entirely
(`submission_or_publication`, never coverable by a standing envelope; see
`consequence-classes.md`).

So there is **no `Public` variant, no wildcard, no "anyone with the link"**: not
a policy that could be relaxed, but something the type cannot express. A test
asserts the serialised form contains no token that reads as open-ended.

## Membership includes currency

`admits(identity, now)` checks the relationship is **still running** as well as
the person being listed. Somebody on a relationship that has ended is not a
member, so access cannot outlive the relationship it came from.

Expiry is inclusive, like every other clock here.

## The kind is part of the identity

`as_key()` is `engagement:acme`, not `acme`. The same company as a **live deal**
and as a **standing client** are different audiences with different rooms and
different registers.

Keying on the id alone would merge two relationships that share a name; tests in
both consumers cover this.

## Not a work context

`work_context` shares two variant names and answers a different question: *what
capability may be used here*, versus *who may see this*. A program is both. They
stay separate so a change to capability resolution never silently moves access
control.

## Supplied, never read

This module reads no store. The roster belongs to whoever owns the relationship
(an engagement store, a programme roster, an account record) and is passed in, so
a room does not depend on which kind of relationship it serves.

## Consumers

- **`data_room`** (`magician-learning/src/data_room/`) — a room belongs to an
  audience. `visible_to` requires **both** clocks to be running (the room's own
  and the relationship's) and refuses an audience the room was not opened for, so
  a roster handed in from elsewhere cannot open it. `data_room_api` opens rooms
  (`POST /data-rooms`) and issues share links
  (`POST /data-rooms/{room_id}/links`). `data_room_reader_api` presents those
  links (`GET /rooms/{room_id}`) against the living audience.
- **`commitments`** — terms get said in every kind of relationship, not only a
  deal. `transcript_claims_api` is the owner surface that records and lists them.
- **`obligations`** — `Obligation` and `RecordObligation` both key on
  `audience: AudienceRef`; there is no `engagement_id` field left in
  `obligations::types`. `work_modules_api` records them
  (`POST /work/obligations`). `introductions::introducer_debts` proposes a
  `RecordObligation` keyed to `AudienceKind::Person` and writes nothing.
