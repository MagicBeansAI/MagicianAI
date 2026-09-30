# Commitments — record, never author

Deal close **§6A**. Plan: `docs/archive/plans/2026-08-07-opc-deal-close.md`.
Module: `magician/src/magician_v2/commitments/`.

§6A: *"Nothing in this set negotiates, and that exclusion is correct. But being
unable to negotiate is not a reason to be **blind** to negotiation happening."*

When terms appear in a conversation — a number, a timeline, a condition, an offer
— they are recorded as a **fact awaiting owner confirmation**.

## The two rules that are the whole feature

1. **It never asserts a commitment outward.** Recording that they offered
   something is not agreeing to it, and the agent may not restate an unconfirmed
   commitment to anyone.
2. **Anything the agent believes *we* committed to requires owner confirmation**
   before any other module treats it as true.

Both reduce to one predicate — `may_be_restated_outward()`, false for everything
except a confirmed entry. One place to get it wrong, one place to check.

That is not a technicality: an agent restating a term the owner never agreed to
has, in effect, **negotiated** — which is the exclusion §6A exists to preserve.

## Confirmation cannot be self-granted

`RecordCommitment` has **no status field**. A machine may only ever record
`Unconfirmed`; there is no parameter and no code path that produces a confirmed
row without a named person confirming it.

`confirm` requires a non-empty `by`. An unnamed confirmation is exactly how an
automated caller would grant itself the single control this register has.

App and revision-aware API callers use `confirm_at_revision`. It compares the
observed revision, binds the complete request to a safe decision id, and appends
the transition and durable receipt in one authoritative JSONL record under a
cross-process decision/target lock. An exact retry returns the original receipt
as `already_applied`; changed actor, target, or revision under the same decision
id is refused. Historical rows deserialize at revision 1 and each accepted
transition increments the revision once.

Decision identity is scope-wide, not audience-shard-local. Before the shard
transition, the store atomically binds the decision id to its complete audience,
target, verb, revision, actor and request fingerprint in a deterministic bounded
index. Receipts carry the complete audience plus the confirmer for confirmation
verbs.

`recover_decision_receipt` is exact lookup for a retry whose signed envelope has
expired: it returns `already_applied` only after the complete binding matches,
`None` when no authoritative receipt exists, and mints no receipt or register
row. Its one write is the idempotent completion-journal append for the recovered
decision — a caller on this route cannot re-enter the mutation path whose replay
branch would heal an interrupted journal write, so without it that completion
would stay invisible to the cursor. `TranscriptIngestion::recover_claim_decision`
repairs its own journal for the same reason.

Decision-index file names are request hashes, not completion order, so they are
never a cursor; the [completion journal](#the-completion-journal) is.

## The query it exists for

§6A: *"a `stated_by_us` entry the owner never confirmed is exactly the thing
worth finding **before the other side does**."*

`unconfirmed_from_us` is that query. The agent believes we promised something and
nobody has checked — and the alternative to finding it is finding out when the
counterparty quotes it back.

`restatable` is its complement: confirmed terms only. A caller composing a
message should build from that and nothing else.

## Lifecycle

`Unconfirmed → Confirmed`, or `→ Superseded` / `→ Withdrawn`. Only the first two
are **live**; neither of the last two may be restated, and **a withdrawn term
cannot be confirmed back into life**. A commitment cannot supersede itself, which
would take a live term out of force with nothing replacing it.

The fold accepts confirmation only from `Unconfirmed`. Receipt-bearing rows are
also checked against their target, verb, actor, fingerprint and exact
expected/resulting revision before they can change the projected state.

## It serves any relationship

Terms get said in procurement, hiring, contract review and support escalation,
not only in a deal. The register binds to an **`AudienceRef`**
(`docs/components/magician/audience.md`) rather than an engagement, and the
audience's kind is part of the register's identity — one company as a live deal
and as a standing client keep separate registers.

## Identity

Derived from `(audience, source, direction, terms)`, with terms normalised for
whitespace and case since re-extraction rarely returns them character-identical.

**The source is in the key deliberately.** The same words in two different
messages are two commitments, because *which message it came from* is what the
owner reads before confirming. Re-extracting the same statement is one.

Every commitment must name its source, for the same reason: the owner confirms by
reading the words, not the summary.

## Production callers

`evidence::transcript_ingestion` turns a conversation into a record: it ingests
utterances, records the act unconditionally, produces `TranscriptClaim`s for
owner confirmation, and converts a confirmed one into a `RecordCommitment`
through `commitment_request_from_claim`. An extractor may not confirm its own
claim.

`TranscriptClaimsApi::ingestion()` constructs that store and is mounted at boot.
The owner surface is `POST /transcripts/ingest`, `GET /transcripts/claims`,
`POST /transcripts/claims/{id}/confirm`, `/reject`, `/commitment`, and
`GET /transcripts/commitments`. Confirmation still requires a named `by` that is
not the extractor.

`GET /transcripts/commitments` reads the register through `for_audience` and
reports `restatable_outward` — the count of rows for which
`may_be_restated_outward()` is true. `Commitments::restatable` is that same
predicate as a query. There is no `for_engagement`.

There is still no automatic meeting-sink feeder: which rooms are outward is a
judgement about the relationship, so the caller supplies the transcript.

## Review and ingest surfaces

Claims Review is OPC's review surface for explicitly ingested transcripts:
`sync_claims` projects the pending register; ordinary Envoy chats and meeting
recordings do not populate it. A confirmed claim may be recorded as a commitment
only through the canonical mapping, and it still lands `Unconfirmed`.

The signed `magician.claims-decision` destination calls the same receipt-bearing
expected-revision methods as the first-party handlers. Package ledgers are
rebuildable projections, never a second authority. Any message composer must
read `restatable`, never the unfiltered register.

### The staged-ingest half

The claims-review package's `stage_ingest` workflow writes an `ingest_request`
row — an explicitly mapped, ordered utterance document, never raw prose for the
host to parse — and cannot apply it.
`claims_decision_contribution::apply_staged_app_ingest` does, through the one
canonical ingest entry, and only that act ref moves the row from `recorded` to
`applied`. It is act-always, so an ingest that yields no candidate still leaves
a durable record of the room; a claim count is never a success signal.

What makes it safe is which mapping comes from where. The document maps a
**speaker key to a named person**: a person's typed intent, and the package's to
state. Mapping a **named person to a side** is the host's, because
`SpeakerAttribution::Ours` is the only attribution that becomes an outward
assertion — a document that could declare its own sides would let an installed
app put words in our mouths. The roster is resolved by the owner's authenticated
session at `POST /apps/installations/{id}/claims-ingests/apply`, and a name it
cannot place refuses the whole request rather than ingesting those words
unattributed: there is no diariser in this path, so an unplaceable name is a
disagreement with the identity register, not a miss. The applier is recorded as
the extractor, which is what keeps the self-confirm rule biting — whoever
applies a room may not then confirm the claims it queued.

The act and the row live in different stores, so they cannot be one write. The
apply is idempotent under one `ingest_id`: a stamp that does not land leaves the
row at `recorded`, and the same request retried resumes the same act instead of
queueing the room twice.

## The completion journal

`evidence::completion_journal` is the scope-wide, append-only record of
**completed** decisions — the claims register's confirm/reject and this
register's receipted record/confirm, in one sequence per
`(principal, workspace)`.

The per-decision index files are named `blake3(decision_id)` and written
*before* their receipt, so a hash-ordered directory cannot be a cursor: a reader
could pass a decision with no outcome yet, and a later decision can sort behind a
live reader and be lost. Journal sequence numbers are assigned at append time,
under one lock, from the journal itself — nothing can be inserted behind a
reader.

Rules a consumer can rely on:

- Sequence numbers start at one and increase by exactly one. **A gap is
  refused**, on every read, rather than skipped: a page that silently jumped a
  seq is indistinguishable from a page that ended.
- A cursor ahead of the head is refused; it means the journal was truncated or
  replaced under a live reader.
- Entries are **locators, not authority**. An entry carries the address of the
  receipt (the audience shard for a commitment, the claim id for a claim), its
  receipt id and request fingerprint — never the outcome. The outcome stays in
  the register record the receipt was written with.
- Only receipted transitions are journalled. `record`, `confirm`, `supersede`
  and `withdraw` without a receipt mint nothing to project.

Ordering: the register row lands first and the journal entry follows, because
journal-first would announce completions that never completed. A crash between
the two is repaired by the retry — every decision path journals on its replay
branch too, and journalling is idempotent by decision id. The visible cost is
that a decision whose journal append fails is reported as an error even though
its register row landed; the caller retries, gets `already_applied`, and the
journal is repaired.

The receipt projector that reads this journal is separate:
`evidence::review_receipt_projection` (`ReviewReceiptProjector`).
