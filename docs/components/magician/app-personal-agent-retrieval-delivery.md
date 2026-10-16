# App personal-agent retrieval delivery

## Atomic source seam

The terminal producer calls
`append_retrieval_projection_in_transaction` with its existing SQLite
transaction, exact `AppScope`, exact scope-binding reference, sealed
`AppPersonalAgentRetrievalProjectionProposalV1`, and the non-deserializable
`AppReviewedContributionFrequencyV1`. The helper consumes the reviewed
per-installation/workflow/action/port frequency bucket and appends the exact
retrieval row without committing. A failure therefore rolls back the terminal
record, frequency consumption, and outbox publication together.

Invalidation uses `append_retrieval_invalidation_in_transaction` with the exact
retained retrieval proposal and sealed `AppMemoryInvalidationV1`. Invalidations
do not consume proposal frequency. Both helpers distinguish exact replay from
substituted bytes; compacted identities fail closed instead of becoming new.

Memory terminal publication can use the same
`consume_contribution_frequency_in_transaction` helper. Its deterministic
frequency event identity includes installation, workflow, action, port, scope,
proposal identity, revision, and digest. Active and previous-window event
ledgers make exact retries free. Pruning advances a per-port issued-time
high-water before deleting bounded replay bytes.

## Delivery chain

V18 uses one ordered retrieval outbox for proposals, invalidations, and
destination expirations. A single move-only lease becomes `dispatching` with
the exact expected destination generation and receipt digest. That predecessor
is retained across response-lost retries. A destination acknowledgement is
accepted only when its generation is predecessor + 1 and its previous receipt
digest equals the retained predecessor.

The high-level `AppPersonalAgentRetrievalProjectionService` is the only adapter
that converts trusted provider configuration and a live reviewed-authority
resolution into the private destination provider/grant fences. API and
workflow callers never receive owner, lease, fence, or acknowledgement types.
The service recovers and audits the destination before draining, and treats a
destination head behind the registry acknowledgement high-water as rollback.
`AppPlatformApi::new` constructs this service only through
`from_current_registry`; provider identity is content-bound to the private
owner/adapter sources, and initialization failure leaves a typed unavailable
lane rather than deriving authority from proposal bytes.

Personal-assistant prompt/search reads create a fresh runtime-owned scoped
identity, reopen the current installation, package, grant, port, and exact
source record before and after the private destination snapshot, then retain
the shared final prompt-eligibility fence. A proposal that merely remains in
the destination store is never sufficient disclosure authority.

The source acknowledgement retains these sufficient fields from the private
destination receipt: receipt ID, operation digest, destination generation,
destination receipt digest, resulting projection digest, recorded time, and
invalidation disposition. Proposal/invalidation IDs and digests remain bound by
the immutable dispatch row and terminal record, so replay never trusts receipt
text to identify source bytes.

## Bounds and expiry

The retrieval source holds at most 10,000 pending events, 4,096 exact source
heads, and 2,048 recent terminal replay records, with a 64 MiB pending-byte
ceiling. Terminal pruning advances a sequence high-water. Frequency buckets use
the shared reviewed maximum of 10,000 proposals per window, a maximum 30-day
window, bounded per-bucket replay bytes, and a bounded/prunable bucket table.

Pending proposals that expire are terminalized without destination delivery.
A live acknowledged proposal that expires generates an ordered expiration row;
the worker calls the retrieval owner's exact expiration operation with the
same destination predecessor chain. Queries also filter expired projections,
so worker delay cannot make expired text prompt-visible.
