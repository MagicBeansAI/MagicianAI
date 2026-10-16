# Commitments — record, never author

Deal close **§6A**. Plan: `docs/archive/plans/2026-08-07-opc-deal-close.md`.
Module: `magician/src/magician_v2/commitments/`.

§6A: *"Nothing in this set negotiates, and that exclusion is correct. But being
unable to negotiate is not a reason to be **blind** to negotiation happening."*

Terms that appear in a conversation (a number, a timeline, a condition, an
offer) are recorded as a **fact awaiting owner confirmation**.

## The two rules that are the whole feature

1. **It never asserts a commitment outward.** Recording that they offered
   something is not agreeing to it, and the agent may not restate an unconfirmed
   commitment to anyone.
2. **Anything the agent believes *we* committed to requires owner confirmation**
   before any other module treats it as true.

Both reduce to one predicate, `may_be_restated_outward()`, false for everything
except a confirmed entry. An agent restating a term the owner never agreed to
has in effect **negotiated**, which is what §6A excludes.

## Confirmation cannot be self-granted

`RecordCommitment` has **no status field**. A machine can only record
`Unconfirmed`; no parameter or code path produces a confirmed row without a named
person. `confirm` requires a non-empty `by`, since an unnamed confirmation is how
an automated caller would grant itself the register's one control.

App and revision-aware API callers use `confirm_at_revision`. It compares the
observed revision, binds the whole request to a safe decision id, and appends
the transition and durable receipt as one authoritative JSONL record under a
cross-process decision/target lock. An exact retry returns the original receipt
as `already_applied`; a changed actor, target or revision under the same
decision id is refused. Historical rows load at revision 1, and each accepted
transition adds one.

Decision identity is scope-wide, not per audience shard. Before the shard
transition, the store atomically binds the decision id to its full audience,
target, verb, revision, actor and request fingerprint in a deterministic bounded
index. Receipts carry the full audience and, for confirmation verbs, the
confirmer.

`recover_decision_receipt` is an exact lookup for a retry whose signed envelope
expired: it returns `already_applied` only when the full binding matches, `None`
when no receipt exists, and mints no receipt or register row. Its one write is
the idempotent completion-journal append for the recovered decision, because a
caller on this route never re-enters the mutation path whose replay would heal an
interrupted journal write. `TranscriptIngestion::recover_claim_decision` repairs
its own journal for the same reason.

Decision-index file names are request hashes, not completion order, so they are
never a cursor; the [completion journal](#the-completion-journal) is.

## The query it exists for

§6A: *"a `stated_by_us` entry the owner never confirmed is exactly the thing
worth finding **before the other side does**."*

`unconfirmed_from_us` is that query: something the agent believes we promised
that nobody has checked. `restatable` is its complement, confirmed terms only; a
caller composing a message builds from that and nothing else.

## Lifecycle

`Unconfirmed → Confirmed`, or `→ Superseded` / `→ Withdrawn`. Only the first two
are **live**; the last two may not be restated, and **a withdrawn term cannot be
confirmed back into life**. A commitment cannot supersede itself, which would
retire a live term with nothing in its place.

The fold accepts confirmation only from `Unconfirmed`. Receipt-bearing rows are
checked against their target, verb, actor, fingerprint and exact expected and
resulting revision before they change the projected state.

## It serves any relationship

Terms are said in procurement, hiring, contract review and support escalation,
not only deals. The register binds to an **`AudienceRef`**
(`docs/components/magician/audience.md`), not an engagement, and the audience's
kind is part of its identity: one company as a live deal and as a standing
client keeps two registers.

## Identity

Derived from `(audience, source, direction, terms)`, with terms normalised for
whitespace and case, since re-extraction rarely returns identical text.

**The source is in the key deliberately.** The same words in two messages are two
commitments, because which message it came from is what the owner reads before
confirming; re-extracting one statement is one. Every commitment must name its
source for the same reason.

## Production callers

`evidence::transcript_ingestion` turns a conversation into a record: it ingests
utterances, records the act unconditionally, produces `TranscriptClaim`s for
owner confirmation, and turns a confirmed one into a `RecordCommitment` through
`commitment_request_from_claim`. An extractor may not confirm its own claim.

`TranscriptClaimsApi::ingestion()` builds that store and is mounted at boot. The
owner surface is `POST /transcripts/ingest`, `GET /transcripts/claims`,
`GET /transcripts/claims/{id}/pending-confirmation`,
`POST /transcripts/claims/{id}/confirm`, `/reject`, `/commitment`,
`GET /transcripts/commitments`, `POST /transcripts/commitments/{id}/confirm`
and `POST /transcripts/counterparty-offers`
(`configure_transcript_claim_routes`). Confirmation needs a named `by` that is not the
extractor.

`GET /transcripts/commitments` reads the register through `for_audience` and
reports `restatable_outward`, the count of rows where
`may_be_restated_outward()` is true. `Commitments::restatable` is the same
predicate as a query. There is no `for_engagement`.

There is no automatic meeting-sink feeder: which rooms are outward is a judgement
about the relationship, so the caller supplies the transcript.

## Review and ingest surfaces

Claims Review is OPC's review surface for explicitly ingested transcripts.
`sync_claims` projects the pending register; ordinary Envoy chats and meeting
recordings do not feed it. A confirmed claim becomes a commitment only through
the canonical mapping, and still lands `Unconfirmed`.

The signed `magician.claims-decision` destination calls the same receipt-bearing
expected-revision methods as the first-party handlers. Package ledgers are
rebuildable projections, never a second authority. Message composers read
`restatable`, never the raw register.

### The staged-ingest half

The claims-review package's `stage_ingest` workflow writes an `ingest_request`
row (an explicitly mapped, ordered utterance document, never raw prose for the
host to parse) and cannot apply it.
`claims_decision_contribution::apply_staged_app_ingest` applies it through the
one canonical ingest entry, and only that act ref moves the row from `recorded`
to `applied`. It always records the act, so an ingest with no candidate still
leaves a durable record; a claim count is never a success signal.

Safety comes from who supplies which mapping. The document maps a **speaker key
to a named person**, the package's to state. Mapping a **named person to a side**
is the host's, because `SpeakerAttribution::Ours` is the only attribution that
becomes an outward assertion; a document that set its own sides would let an
app put words in our mouths. The owner's authenticated session resolves the
roster at `POST /apps/installations/{id}/claims-ingests/apply`, and a name it
cannot place refuses the whole request: there is no diariser on this path, so an
unplaceable name is a conflict with the identity register. The applier is
recorded as the extractor, so whoever applies a room may not confirm the claims
it queued.

The act and the row live in different stores and cannot be one write. The apply
is idempotent under one `ingest_id`: a stamp that does not land leaves the row
at `recorded`, and a retry resumes the same act instead of queueing the room
twice.

## The completion journal

`evidence::completion_journal` is the scope-wide, append-only record of
**completed** decisions: the claims register's confirm/reject and this
register's receipted record/confirm, in one sequence per
`(principal, workspace)`.

Per-decision index files are named `blake3(decision_id)` and written *before*
their receipt, so a hash-ordered directory cannot be a cursor: a reader could
pass a decision with no outcome yet, or miss one that sorts behind it. Journal
sequence numbers are assigned at append time, under one lock, from the journal
itself, so nothing lands behind a reader.

- Sequence numbers start at one and rise by exactly one. **A gap is refused** on
  every read, never skipped, since a page that jumped a seq looks like a page
  that ended.
- A cursor ahead of the head is refused: the journal was truncated or replaced
  under a live reader.
- Entries are **locators, not authority**: the receipt's address (audience shard
  for a commitment, claim id for a claim), receipt id and request fingerprint,
  never the outcome, which stays in the register record.
- Only receipted transitions are journalled. `record`, `confirm`, `supersede`
  and `withdraw` without a receipt produce nothing.

The register row lands first and the journal entry follows, since journal-first
would announce completions that never happened. A crash between the two is
repaired by the retry: every decision path also journals on its replay branch,
idempotently by decision id. The visible cost: a decision whose journal append
fails reports an error though its row landed; the retry gets `already_applied`
and repairs the journal.

The receipt projector that reads this journal is separate:
`evidence::review_receipt_projection` (`ReviewReceiptProjector`).
