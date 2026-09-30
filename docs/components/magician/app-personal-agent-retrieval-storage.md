# App personal-agent retrieval storage

See [App personal-agent retrieval delivery](app-personal-agent-retrieval-delivery.md)
for the V18 source journal and worker boundary.

The retrieval destination is physically and semantically separate from agent
memory. It stores the closed
`AppPersonalAgentRetrievalProjectionProposalV1` body and never converts it into
a memory candidate, memory tier record, embedding, or ranked result.

## Authority and identity

Only server code can construct the provider identity and reviewed-grant fence;
neither type is deserializable. Every stage is bound to all of the following:

- authenticated principal, workspace, and scope-binding reference;
- scoped private `AgentStorage` root;
- installation and installation generation;
- package content, reviewed grant, and contribution-port digests/revisions;
- retrieval provider ID, provider revision, provider authority digest, and
  projection-schema digest;
- the sealed proposal's exact canonical source reference, revision, digest,
  dedupe key, target agent/goal, purpose, audience, and handling labels.

**Provider authority digest.** The provider authority digest is derived from
the explicit provider ID, `RETRIEVAL_PROVIDER_REVISION`, and a fixed authority
seed, never from source bytes — hashing source would change every scope's owner
identity on any edit and wedge boot repair. Recovery re-seeds a head that is still at generation 0 with no checkpoint under the
current owner and logs both digests; a sealed set (generation > 0) under
another owner still fails closed. Bump the revision only together with a
migration for sealed sets.

The owner accepts exactly one source and only
`replace_exact_source_head` plus `tombstone_on_any_source_drift`. Equal-revision
byte substitution and source/proposal rollback fail closed.

## Worker-only delivery seam

The raw owner module and its re-exports are crate-private. The registry-backed
projection worker adapter, rather than an API or workflow handler, constructs the trusted
provider identity and reviewed-grant fence. It then recovers the exact optional
destination head and calls `stage_projection` or `invalidate_projection` with
the sealed source DTO, expected head, and server receipt time. The fence also
binds the reviewed target agent and optional goal.

An initial delivery expects no head. Every later delivery carries the exact
prior generation and receipt digest. `Applied` versus `ExactReplay`, together
with the original source DTO and destination receipt's ID, generation,
invalidation disposition (when present), receipt digest, projection digest,
owner digest, and recorded time, is sufficient to seal a source
acknowledgement. A replay returns the identical destination receipt; raw
destination types never need to cross an API boundary.

## Durable chain and recovery

Files live below the scoped owner root at
`app-contributions/personal-agent-retrieval-v1/`. Directories and files are
private, owner-checked, bounded, and accessed without following final symlinks.
Mutations use this order:

1. write the immutable, domain-separated receipt;
2. advance the exact owner-bound head;
3. write the derived typed projection;
4. periodically seal a checkpoint and prune only receipts older than the
   retained 64-generation replay window.

Recovery revalidates every seal and identity, replays at most 64 receipts, and
may adopt only the single exact receipt immediately after the durable head.
Missing prefixes, substituted generations, mismatched predecessor digests, and
head/projection rollback fail closed. Exact recent replay returns the original
receipt; replay older than the retained window reports history compaction.

A private read accepts a file only when it is a regular file, owned by this
euid, with one link and a size inside the caller's ceiling, reached through a
parent chain every level of which was proven euid-owned and `0700`. Those five
conditions fail closed. The mode of the leaf itself does not: a file that
satisfies all of them but sits at a laxer mode is tightened to `0600` on the
descriptor already open, and the read proceeds. Why: the parent chain decides
who could have written the file, and `write_private_bytes_atomic` renames before
it chmods, so a crash leaves exactly this state; refusing the read would block
the rewrite that repairs it, forever. A mode that cannot be tightened still
fails closed.

## Invalidation, expiration, and query

Invalidation reuses the typed contribution invalidation DTO and must match the
stored proposal, canonical source, scope, grant, and provider identities.
Terminal heads are retained as bounded high-water records while their text is
removed from the live projection. Expiration has its own receipt operation.

The typed query projection requires the exact provider, installation
generation, package digest, and current grant revision/digest; caps results at
128; and matches the target agent and optional goal. It excludes expired
proposals against server wall time even before the durable expiration operation
is delivered, closing lifecycle-delivery lag. The query does no ranking or
embedding. The mounted personal-assistant prompt/search adapter preserves the
same live scope, grant, provider, source, and expiration fences on both sides
of its private snapshot and again at the shared final prompt-eligibility
boundary.
