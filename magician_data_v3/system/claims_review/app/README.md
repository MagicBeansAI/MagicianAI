# Claims review package boundary

This directory is the system-class UI package for claims and commitments
review. The manifest class is a claim until host-controlled, digest-pinned boot
admission establishes trusted provenance; ordinary staging/publication rejects
it. The package contains no host transition implementation and owns no
authoritative evidence state.

## What the package owns

- Bounded projections of claim summaries/details, commitments, correction
  history, and entity context.
- A `review_decision` request ledger.
- A `review_receipt` projection populated only from durable signed destination
  receipts.
- An `ingest_request` staging ledger.
- Declarative companion views and the reviewed `surfaces/review.html` console.
- A bounded native pending-claims widget and first-party navigation declaration;
  both remain inert until their server-owned runtime/admission consumers bind a
  live installation generation.

## What remains host-owned

- Pending claim and commitment registers.
- Claim/commitment revision checks and terminal-state rules.
- Extractor-self-confirm refusal and named-person commitment confirmation.
- `TranscriptIngestion`, including its durable act row.
- Owner signing and application through `magician.claims-decision`.

The distinction is visible in the data model: a local decision remains an
immutable `apply_state: recorded` source head. Mutating it after owner signing
would make an exact receipt retry stale. Applied/idempotent/refused status,
canonical decision id, and authenticated actor are projected only into a
separate `review_receipt` from the durable destination receipt. No UI state,
local source row, claim count, or workflow completion substitutes for it.

## Governed action inputs

| Action | Exact input |
|---|---|
| `confirm_claim` | `request_id`, `claim_id`, `expected_revision`, `reason` |
| `reject_claim` | `request_id`, `claim_id`, `expected_revision`, `reason` |
| `record_commitment` | `request_id`, `claim_id`, `expected_revision` |
| `confirm_commitment` | `request_id`, `commitment_id`, `audience_kind`, `audience_id`, `expected_revision` |
| `stage_ingest` | `ingest_id`, `transcript_text`, `speaker_mapping_json`, `audience_kind`, `audience_id`, `outwardness_reason` |

The decision workflows intentionally use no tool and declare no
`contribution_ports`: they create reviewable source records. The separately
reviewed signed destination seam is the only apply path. No current package or
host consumer automatically turns those source records into signed envelopes.
The authenticated host derives the signed proposal actor from `actor_ref` and
projects it only with the receipt; it never rewrites the signed source head.
Neither a display name nor any frame-supplied string is actor authority.

## Ingest document

`speaker_mapping_json` has exactly this logical shape:

```json
{
  "speakers": {
    "owner": "Named owner",
    "client": "Named client"
  },
  "utterances": [
    { "speaker": "owner", "text": "Exact words" },
    { "speaker": "client", "text": "Exact words" }
  ]
}
```

Every utterance speaker must exist in `speakers`. The console accepts a manual
`speaker-key = Named Person` map plus explicit `speaker-key | exact words`
utterances and constructs the document; it never assigns speakers by guessing
from raw prose.

When the host accepts the request, `TranscriptIngestion` writes a durable act
even if extraction returns zero claims. That act-always property makes empty
extraction distinguishable from an ingest that never happened.

## Frame posture

The custom surface reads only package projections. Evidence/entity sync always
names one explicit agent id, and commitment sync always names one audience;
neither path unions a workspace or performs an unbounded all-shard scan. Its
bridge client caps a session at 24 messages and each query at 20 records. Every
rendered string uses DOM text nodes. It uses the public `launch_action` method
over the parent-frame web transport or the direct iOS message handler, and
accepts replies through either host callback. A launch contains only the action
name, a stable per-intent idempotency key, and the exact reviewed input; the
authenticated host resolves mutable schema, grant, policy, scope, and
provenance fields immediately before admission. The frame displays the
host-owned run reference and never fabricates authority.

The console does not bulk-read transcript-bearing claim details: it requests
only the selected claim with an exact equality predicate and a one-row limit,
then caches that row for the frame session. Evidence and activity queries omit
source provenance, decision payload JSON, and other fields the surface does not
render. Deleted evidence tombstones are excluded by the host read provider.

## Bounded paging and exact words

Version 0.2.11 persists source page membership/cursors atomically with summaries
in `claim_sync_page`. The frame queries only those IDs; next/first page controls
launch the declared read-only sync. Selected detail also refreshes its summary
status/revision, and Refresh invalidates detail caches. Exact statement words
are complete text nodes in a scrollable block, never silently shortened.
