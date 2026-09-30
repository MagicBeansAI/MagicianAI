# Audience — who may see something

Module: `magician/src/magician_v2/audience/`.

A **named, enumerable set of identities with a lifetime.**

## Why it exists

The data room bound its access to an **engagement** — bilateral, external,
counterparty-shaped. That single coupling is what made the whole deal-close set
read as a fundraising feature, when the machinery underneath is not:

> a bounded set of documents shared with a bounded set of people, whose access
> derives from a relationship and ends with it

describes cohort materials, a client's deliverables, an audit pack and somebody's
own records equally well. Widening the binding from *engagement* to **audience**
is the whole generalisation. Everything else was already generic.

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

So there is **no `Public` variant, no wildcard, no "anyone with the link"** — not
as a policy that could be relaxed, but as something the type cannot express. A
test asserts the serialised form contains no token that reads as open-ended.

That matters *more* after this generalisation than before it: "audience" sounds
like it could be open-ended in a way "engagement" never did, and the refusal has
to survive the rename.

## Membership includes currency

`admits(identity, now)` checks the relationship is **still running** as well as
the person being listed. Somebody on a relationship that has ended is not a
member — treating "was listed" as "may see" is how access outlives the
relationship it came from, which is the failure the whole model prevents.

Expiry is inclusive, like every other clock here.

## The kind is part of the identity

`as_key()` is `engagement:acme`, not `acme`. The same company as a **live deal**
and as a **standing client** are different audiences with different rooms and
different registers.

Keying on the id alone would have merged two relationships that happen to share a
name — the specific way widening this binding could have gone wrong, and there
are tests for it in both consumers.

## Not a work context

`work_context` shares two variant names and answers a different question: *what
capability may be used here*, versus *who may see this*. A program is both, which
is exactly why they stay separate — unifying would couple capability resolution
to access control, and a change to one would silently move the other.

## Supplied, never read

This module reads no store. The roster belongs to whoever owns the relationship —
an engagement store, a programme roster, an account record — and taking a
dependency on one would tie every consumer to that single source. The whole point
is that a room does not care which kind of relationship it is serving.

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
