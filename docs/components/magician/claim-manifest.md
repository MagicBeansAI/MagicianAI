# Claim manifest — which claims an artifact revision carries

The generic substrate of Composable Work Modules, **Module B** (*"produce a
persuasive artifact from a claim set, with provenance"*).
Module: `magician/src/magician_v2/claim_manifest/`.

## Built vs sent

The outward assertions store answers what was **sent**; this answers what was
**built**. The assertions plan's own motivation names *"the decks that show it,
or the drafts still carrying it"* — drafts are exactly what a sent-record
cannot find. Presentation Maker and Demo Maker consume this; they never keep
their own copy (assertions §2 forbids a second authoritative record).

**Reachable from `work_modules_api`.** `POST /api/magician/v2/work/claim-manifests`
binds a revision's claim set, `GET ?artifact_ref=[&revision_ref=]` reads the
revision history of what an artifact claimed, and
`GET /work/claim-manifests/carrying?claim_ref=[&latest_only=]` is the
correction-propagation query. There is still no Presentation Maker or Demo Maker
in the tree, so the paragraph above states the ownership rule those consumers
must follow when they arrive rather than a relationship that exists today — but
any flow that produces an artifact can now bind and read a manifest over HTTP
without becoming one.

## A revision's claim set is immutable

Rebinding the same `(artifact, revision)` with identical claims resumes
idempotently; with different claims it is **refused** — revisions exist
precisely so content cannot change under a reference; the fix is a new
revision. A binding with no evidence refs is refused (*"never answer from
nothing"* applies to decks as much as forms), as is a claim bound twice in one
manifest — double-binding makes its evidence backing ambiguous exactly during
correction propagation.

## The correction-propagation query

`revisions_carrying(claim)` — when a figure is corrected, these are the decks
and drafts still showing it. The reverse index is written **before** the row,
and index lines are JSON-encoded so a ref containing a newline cannot shear the
format. `latest_revision_carrying` separates *"still carrying it now"* from
*"carried it once"*: an artifact whose newest revision dropped the claim is the
fixed case and is not flagged.

Failure semantics from `magician_v2::jsonl`: an unreadable log is an error, not
an empty store — a silently empty manifest is a deck the correction query never
finds.
