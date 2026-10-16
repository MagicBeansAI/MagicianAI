# Governed app memory and retrieval contribution storage

A reviewed app workflow can propose a memory candidate or a personal-agent
retrieval projection from a mutation it just made. It never writes canonical
memory. Memory proposals are staged durably, inspected in trusted desktop
Settings, and accepted, rejected or revoked only through the code-verified
Keychain desktop identity; accepted ones reach prompt retrieval only after
fresh installation, grant, entity-revision, handling-policy and provider
checks. Retrieval delivery is covered in
[App personal-agent retrieval delivery](app-personal-agent-retrieval-delivery.md)
and [storage](app-personal-agent-retrieval-storage.md).

## Manifest, lock, review, and task authority

`contribution_ports_v1` is a required manifest feature. Each port belongs to
one workflow and declares a closed source, destination, purposes, audience,
selected fields, evidence classes, proposal-frequency ceiling and maximum
retention. V1 admits only `mutation_backed_entity_projection`: the workflow
must return and mutate exactly one declared entity, and every selected field
must exist on it. Standalone values, prompt text, arbitrary queries, artifacts,
multi-entity and non-mutating projections are rejected.

The two V1 destination bodies are fixed, not app-selectable:

- `memory` → user tier, `Knowledge` semantic destination, audience `user:owner`;
- `personal_agent_retrieval` → `personal-assistant`, no goal, audience
  `agent:personal-assistant`.

The package lock seals the port, destination body, workflow and result digests
and every action that selects the workflow; the port is part of the lock
digest. Older hosts reject the unknown feature; review rejects a missing,
extra, reordered or substituted lock binding.

Approval may deny a port or narrow fields, purposes, evidence classes,
frequency and retention; it cannot change the source entity, destination or
audience. Each selected port needs its exact reviewed-grant digest. The sorted
approved set is folded into the grant authority digest, so omission or
substitution changes the identity workflow launch uses.

The task contribution binding is non-deserializable. It is minted only from
the current non-revoked grant, its consumed approval and the package lock, and
seals task and installation generation, package, grant, approval, workflow and
action digests, port, destination and result contract. Recovery rebuilds and
compares the whole binding; names or serialized fields never become authority.

## Ownership boundary

An app never writes canonical memory, sets rank/heat/salience/confidence, or
decides its own proposal. `magician-app-contract::contribution` defines two
closed bodies (memory-candidate proposal, personal-agent retrieval projection)
that share only source, settlement, package/grant/schema, handling-label and
evidence identity. There is no universal JSON contribution body.

The source registry owns the V17 memory journals and V18 shared-frequency and
retrieval journals:

- `app_memory_contribution_outbox`: sealed candidate bytes and source
  identities beside a settlement reference; every claim rederives the row and
  the current source head;
- `app_memory_invalidation_outbox`: source-drift tombstones;
- per-installation dedupe heads implement the only V1 update policy,
  `ReplaceExactSourceHead`; terminal rows keep identities in a 1,024-row
  exact-replay window, and older retries get typed `HistoryCompacted`.

## Feedback and model-output safety

The V1 producer is an agent-written terminal summary, so new ports require
evidence class `hypothesis`; `derived` and `authoritative` are denied.
Historical sealed rows are still projected as non-authoritative hypothesis.

Each hypothesis is bound to the mutation receipt, record revision, fields,
source and provenance digests and a finite TTL. The destination bodies have no
confidence, heat, salience, rank or temperature fields, so model numbers cannot
become scores. Reflections and other free-form model output have no authority
surface.

In the prompt, accepted app hypotheses go in the `SourceEvidence` lane with
`hypothesis`/`non_authoritative` metadata and an instruction to verify against
current evidence. They cannot occupy the user-preference lane or override user
instructions, authoritative records or policy, and still pass final source,
retention, grant, installation and model-processing checks.

## Source journal invariants

- **Quotas never gate safety.** Admission quotas count only live unexpired
  proposals; invalidation, decision, expiry, tombstone and compaction are never
  gated. At most 4,096 distinct source heads; each V17 table has row and byte
  accounts.
- **Per-head invalidation serialization.** A leased command may keep one next
  pending command, upgraded only to the highest source revision, so newer
  commands are never dropped and older retries get `HistoryCompacted`. A new
  proposal cannot advance a head whose prior invalidation is unacknowledged.
- **Expiry never assumes unspent.** It journals `ContributionExpired`, keeps
  the sealed proposal and compacts only after the destination's invalidation
  ack. A replacement atomically journals the prior head's invalidation and
  publishes the new head. Other update policies are denied.
- **Leases.** Claims rederive every identity and digest, then move `leased` →
  `dispatching` under owner, token, epoch and expiry, producing a move-only
  dispatch permit, the only thing that reaches the destination. Source ack
  consumes that identity and is idempotent. Expired dispatches are reclaimable
  only at a newer epoch.
- **Size ceilings.** Invalidations and source acks: 64 KiB each; a destination
  receipt may hold one 256 KiB proposal plus a 64 KiB envelope.
- **Ack revalidation.** The source transaction reparses the payload and checks
  evidence, scope, head and invalidation lineage before publishing an ack.
- **Ack authentication.** Source advancement consumes a move-only, non-Serde
  permit minted only by the destination owner; a recomputed digest is never
  destination authentication.
- **Scope.** Stage and invalidate seams accept only a live lease claimed under
  the registry's authenticated principal/workspace and scope binding,
  rechecked at every step. The scoped AgentMemory owner rejects cross-scope
  lease reuse.

**Terminal producer.** The workflow terminal owner rebuilds every binding from
grant, approval and lock before entity I/O and again before publication. The
result timestamp is fixed in the pre-I/O intent, so result, mutation receipt,
source revision, labels, port and destination determine the proposal bytes.
Run state, frequency consumption and outbox rows commit in one SQLite
transaction, so recovery regenerates identical bytes. A candidate over its
frequency window, with compacted frequency history, or already expired is
omitted without withholding the workflow result.

## Destination durability

`AgentMemoryService` owns a private `app-contributions/memory-v1` directory.
One dispatched operation publishes in order:

1. immutable source-dispatch replay material for the next generation;
2. an immutable, generation-numbered destination receipt;
3. the generation head and predecessor digest;
4. the derived projection;
5. every 64 generations, a digest-checked checkpoint, then fsynced pruning of
   the superseded receipts and dispatch material.

An in-process mutex precedes a private cross-process file lock; no registry
transaction is held while taking it. Reads are byte-bounded and no-follow, and
Unix checks require private `0700` dirs and `0600` files. Recovery adopts only
the exact `head + 1` receipt; dispatch material without its receipt is removed.
At most 127 receipt files exist between checkpoints and only the latest 64
operation identities promise exact replay. Each of the 4,096 source heads keeps
its latest proposal and invalidation slots, so a response-lost dispatch can
re-mint its ack after pruning. Gaps, predecessor substitution, receipt
mutation, digest drift and quota overflow fail closed.

Every dispatch carries the source's acknowledged destination generation and
digest; the destination must contain that prefix. An empty or rolled-back store
is accepted only when the source has no acknowledged head. A newer checkpoint
may stand in for a pruned receipt only when a dispatch high-water binds it. A
destination ahead of the source is kept as response-loss evidence, never reset.

The projection holds only live proposed and accepted entries plus per-source
high-water. Rejected, replaced and invalidated entries collapse into the
high-water (reason, identity, revision, time, retention ceiling, no claim
text). An invalidation that arrives before its proposal records
`SupersededBeforeAdmission`; a newer `ReplaceExactSourceHead` proposal arriving
first retracts the prior head, and the later invalidation settles
`AlreadyTombstoned`. Only the projection worker reaches this seam; it is not a
general accept/reject/revoke API.

**Byte budget.** Live proposals have a 4 MiB admission budget; 4,096 compact
heads reserve 8 KiB each plus replay metadata. Existing-source transitions
always fit; only a new live head can hit quota. A later proposal may replace a
compact tombstone only at strictly greater proposal and source revisions.

## Trusted owner decision and prompt use

An interactive authenticated session sees a bounded list of unexpired
proposals. Each review embeds the sealed proposal, destination generation and
predecessor digest and the current desktop identity under a display digest.
Desktop Settings shows source, fields, claim, purpose, audience, labels, head,
identity and digest before enabling a decision.

The native Tauri command is a typed signer, not a generic Keychain oracle: it
rebuilds the accept/reject/revoke envelope, requires the confirmed display
digest and the macOS-pairing-pinned desktop identity, and signs
domain-separated bytes. The destination verifies signature and head before
promotion; substituted bytes fail closed. Accept retains no later than the
proposal expiry. Revoke applies only to the currently accepted proposal at the
shown head, removes prompt eligibility in the same receipt and compacts the
source as `owner_revoked`. A web session never gets a signing command or
memory-mutation API.

`GET /api/magician/v2/apps/memory-contributions/state` is an interactive,
scope-bound, no-store read model joining the memory owner's
proposed/accepted/rejected/stale/tombstoned projection with the retrieval
owner's live/invalidated/expired heads (32 items and a byte ceiling each). The
Apps page shows source reason and retention only; desktop Settings also offers
revoke for accepted memory rows.

Accepted entries are not copied into the legacy mutable candidate table. At
prompt retrieval they become ephemeral source-linked candidates and pass the
same installation, package, grant, revision, field, label, model-processing and
provider checks as legacy app memory. Destination receipt state is the only
acceptance owner; expired, invalidated, drifted, revoked or ineligible entries
disappear.

## Invalidation sources and purge

Source mutation, record forget, reviewed update or reinstall, disable,
quarantine, uninstall-retain and grant revoke append invalidations in their
own atomic source transaction. Hard record forget journals both memory and
retrieval `SourceForgotten` in the same transaction as physical deletion.

For retrieval, core rebuilds the reviewed port/grant/provider fence from
registry authority for each new admission (proposal bytes cannot mint it); a
response-lost dispatch may replay only its byte-identical operation before the
safety invalidation.

Record retention refuses whole-installation selection; that belongs to the
multi-store purge coordinator, which takes a server-issued preview and an exact
durable confirmation. It inventories every Storage Governance target, refuses
while memory, retrieval or lifecycle delivery is unacknowledged, removes
installation-owned rows in one registry transaction, advances the installation
to `purged`, and keeps a replayable 17-target receipt. SQLite WAL, shared
package/Artifact content, audit-policy rows and provider history are disclosed
as retained or unknown, not claimed erased. The legacy candidate store is not
the contribution destination owner.
