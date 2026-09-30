# Governed app memory and retrieval contribution storage

A reviewed app workflow can propose a memory candidate or a personal-agent
retrieval projection from a mutation it just made. It never writes canonical
memory. Memory proposals are durably staged, inspected in trusted desktop
Settings, and accepted, rejected or revoked only through the code-verified
Keychain desktop identity; accepted ones join prompt retrieval only after fresh
installation, grant, entity-revision, handling-policy and provider checks.
Retrieval delivery is covered in
[App personal-agent retrieval delivery](app-personal-agent-retrieval-delivery.md)
and [storage](app-personal-agent-retrieval-storage.md).

## Manifest, lock, review, and task authority

`contribution_ports_v1` is an explicit required manifest feature. Each port is
local to one workflow and declares a closed source, destination, purposes,
audience, selected fields, evidence classes, proposal-frequency ceiling and
maximum retention. V1 admits only `mutation_backed_entity_projection`: the
workflow must return and mutate exactly one declared entity, and every selected
field must exist on it. Standalone values, prompt text, arbitrary queries,
artifacts, multi-entity and non-mutating projections are rejected, never
inferred as source evidence.

The two V1 destination bodies are fixed, not app-selectable:

- `memory` → user tier, `Knowledge` semantic destination, audience `user:owner`;
- `personal_agent_retrieval` → `personal-assistant`, no goal, audience
  `agent:personal-assistant`.

Package locking seals the port declaration, fixed destination body, workflow
declaration digest, result digest, and every action declaration that selects
the workflow; the port is part of the package-lock digest. Older hosts reject
the unknown feature; review rejects a missing, extra, reordered or substituted
lock binding.

Installation review returns the requested port ceilings. Approval may deny a
port or narrow fields, purposes, evidence classes, frequency and retention; it
cannot change the source entity, destination body or audience. Every selected
port needs its exact reviewed-grant digest; unknown/duplicate selections are
rejected. The sorted approved set is stored on the consumed approval and folded
into the installed grant authority digest, so omission or substitution changes
the identity workflow launch uses.

The task contribution binding is non-deserializable. It is minted only from the
current non-revoked grant, its consumed approval and the immutable package lock,
and seals task/installation generation, package revision/content/lock, grant
revision/authority, approval, workflow and action declaration digests, locked
port, reviewed grant, fixed destination and result contract. Recovery
reproduces and compares the whole binding from those owners; names or
serialized fields never become authority.

## Ownership boundary

An app never writes canonical memory, sets rank/heat/salience/confidence, or
decides its own proposal. `magician-app-contract::contribution` defines two
closed bodies (memory-candidate proposal, personal-agent retrieval projection)
sharing only bounded source, settlement, package/grant/schema, handling-label
and evidence identity. There is no universal JSON contribution body.

The source registry owns the V17 memory journals and V18 shared-frequency and
retrieval journals plus bounded high-water/replay state:

- `app_memory_contribution_outbox`: exact sealed candidate bytes and normalized
  source identities beside a canonical settlement reference; every claim
  rederives that row and the current sealed source head;
- `app_memory_invalidation_outbox`: exact source-drift tombstones;
- per-installation/dedupe heads implement the only V1 update policy,
  `ReplaceExactSourceHead`; compact terminal rows keep immutable identities in a
  1,024-row exact-replay window. Older exact retries get typed
  `HistoryCompacted`, not a false byte-identical promise.

## Feedback and model-output safety

The V1 producer is an agent-written terminal summary, so admission, lock
validation and publication require evidence class `hypothesis`; `derived` and
`authoritative` are denied for new ports. Historical sealed rows stay readable
but are still projected as non-authoritative hypothesis.

Each hypothesis is bound to the mutation receipt, record revision, selected
fields, canonical source/provenance digests and a reviewed finite TTL. The
destination bodies have no confidence/heat/salience/rank/temperature fields, so
model numbers cannot become scores. Reflections and other free-form model
output have no separate authority surface.

At the prompt boundary, accepted app hypotheses go in the `SourceEvidence` lane
with `hypothesis`/`non_authoritative` metadata and a system instruction to
verify against current evidence. They cannot occupy the user-preference lane or
override user instructions, authoritative records or active policy, and still
pass final current-source, retention, grant, installation and model-processing
revalidation.

## Source journal invariants

- **Quotas never gate safety.** Admission quotas count only live unexpired
  proposals; invalidation, decision, expiry and tombstone transitions are never
  gated, nor is bounded compaction. At most 4,096 distinct source heads; each
  V17 table has explicit row and byte accounts.
- **Per-head invalidation serialization.** An immutable leased/dispatching
  command may retain one next pending command, upgraded only to the maximum
  source revision; the replaced identity survives as high-water, so newer
  commands are never dropped and older retries get `HistoryCompacted`. A new
  proposal cannot advance a head whose prior invalidation is unacknowledged.
- **Expiry never assumes unspent.** It journals `ContributionExpired`, keeps
  the sealed proposal and evidence, and compacts only after the destination's
  exact invalidation ack. A replacement validates its new head, then atomically
  journals the prior-head invalidation, terminalizes the prior payload and
  publishes the new proposal/head. Unsupported update policies are denied.
- **Leases.** Claims rederive every SQL identity and payload digest, then move
  `leased` → `dispatching` under owner, token, epoch and absolute expiry,
  producing a move-only dispatch permit — the only thing that reaches the
  destination. Source ack consumes the same dispatch identity and is
  idempotent. Expired dispatches are reclaimable only at a newer epoch.
- **Size ceilings.** Invalidations and source acks: 64 KiB each; a destination
  receipt may hold one 256 KiB proposal plus a 64 KiB envelope.
- **Ack revalidation.** Before ack publication the source transaction reparses
  the payload and revalidates evidence, scope, current/tombstoned head and
  monotonic invalidation lineage; a newer applied invalidation may be ahead only
  when that lineage proves it.
- **Ack authentication.** Source advancement consumes a move-only, non-Serde
  permit minted only by the destination owner; a recomputed content digest is
  never destination authentication.
- **Scope.** Stage/invalidate seams accept only a live move-only lease claimed
  under the registry's authenticated principal/workspace and opaque scope
  binding, rechecked at claim, begin, release, ack and mutation and sealed into
  invalidation documents. The scoped AgentMemory owner rejects cross-scope lease
  reuse.

**Terminal producer.** The workflow terminal owner reconstructs every binding
from grant, approval and lock before entity I/O and again before publication. It
fixes the result timestamp in the pre-I/O commit intent and derives one
revision-specific sealed source digest over a stable record reference; result,
mutation receipt, source revision, labels, port and destination determine the
proposal bytes. Run-state generation, per-port frequency consumption and outbox
rows commit in one SQLite immediate transaction, so response-loss recovery
regenerates identical bytes. A candidate whose frequency window is full (or
whose frequency history was compacted), or that is already expired before
publication, is omitted without withholding the workflow result.

## Destination durability

`AgentMemoryService` owns a private `app-contributions/memory-v1` directory. One
source-dispatched operation publishes in order:

1. immutable source-dispatch replay material for the next generation;
2. an immutable, generation-numbered destination receipt;
3. the generation head and predecessor digest;
4. the derived projection;
5. every 64 generations, a digest-checked checkpoint, then fsynced pruning of
   the superseded receipt window and its source-dispatch material.

An in-process mutex precedes a private cross-process file lock; no registry
transaction is held while taking it. Reads are byte-bounded and no-follow; Unix
checks require private `0700` dirs and `0600` regular files. Recovery adopts only
the exact `head + 1` receipt; next-generation dispatch material without its
receipt is uncommitted and removed under the lock (never after its receipt
exists). Recovery rebuilds at most 64 projection generations; at most 127
receipt files exist between checkpoints; only the latest 64 operation identities
promise exact replay. Each of the 4,096 source heads separately keeps its latest
proposal and invalidation slots plus compact receipt/ack material, so a
response-lost dispatch can re-mint its ack after the general window is pruned.
Gaps, predecessor substitution, receipt mutation, checkpoint/projection digest
drift and quota overflow fail closed.

Every dispatch carries the source's acknowledged destination generation/digest;
the destination must contain that exact prefix. An empty or rolled-back store is
accepted only when the source has no acknowledged head. A newer checkpoint may
stand in for a pruned acknowledged receipt only when one source-dispatch
high-water binds that generation, digest and lineage. A destination prefix ahead
of the source is kept as response-loss evidence, never reset.

The projection holds only live proposed/accepted entries, bounded per-source
high-water and recent replay. Rejected, replaced and invalidated entries collapse
into the source high-water (owner/source reason, identity, event revision,
state-change time, retention ceiling — no rejected or tombstoned claim text).
An invalidation arriving before its proposal records `SupersededBeforeAdmission`;
a newer `ReplaceExactSourceHead` proposal arriving first retracts the prior head
in the same receipt, and the later invalidation settles `AlreadyTombstoned`.
Owner decisions need a separate decision-receipt digest; invalidation must match
the proposal's sealed digest and source identity at a newer revision. Only the
bounded projection worker reaches this seam; it is not a general
accept/reject/revoke API.

**Byte budget.** Live proposals have a separate 4 MiB admission budget; 4,096
compact heads reserve 8 KiB each plus fixed replay metadata. Existing-source
transitions therefore always fit; only a new distinct live head can hit quota. A
settled invalidated head is a compact tombstone; a later proposal may replace it
only at strictly greater proposal and source revisions.

## Trusted owner decision and prompt use

An interactive authenticated session sees a bounded list of unexpired proposed
entries. Each review embeds the sealed proposal, destination
generation/predecessor digest and current desktop identity, covered by a
display digest. Desktop Settings renders source, fields, claim, purpose,
audience, labels, head, identity and digest before enabling a decision.

The native Tauri command is a typed signer, not a generic Keychain oracle: it
rebuilds the accept/reject/revoke envelope, requires the confirmed display
digest and the macOS-pairing-pinned desktop identity, and signs domain-separated
bytes. The destination verifies signature and current head before promotion.
Exact response-loss replay is adopted before new-head/expiry validation;
substituted bytes fail closed. Accept retains no later than the reviewed
proposal expiry; reject/revoke carry no retention deadline. Revoke applies only
to the currently accepted proposal at the displayed head, removes prompt
eligibility in the same receipt, and compacts the source as `owner_revoked`. A
web session never gets a signing command or generic memory-mutation API.

`GET /api/magician/v2/apps/memory-contributions/state` is an interactive,
scope-bound, no-store read model joining the memory owner's
proposed/accepted/rejected/stale/tombstoned projection with the retrieval owner's
live/invalidated/expired heads, under independent 32-item and byte ceilings. The
Apps page shows source reason and retention only; desktop Settings also gets a
current-head revoke review for accepted memory rows.

Accepted entries are not copied into the legacy mutable candidate table. At
prompt retrieval they are converted ephemerally to the source-linked candidate
contract and pass the same installation, package, grant, entity revision,
field, label, model-processing and provider checks as legacy app memory.
Destination receipt state is the sole acceptance owner; expired, invalidated,
drifted, revoked or ineligible entries disappear.

## Invalidation sources and purge

Source mutation, explicit/cascade record forget, reviewed update/reinstall,
disable, quarantine, uninstall-retain and grant revoke append
destination-specific invalidations in their existing atomic source transaction.
Hard record forget journals both memory and retrieval `SourceForgotten` inside
the same SQLite transaction as physical deletion, keeping the next logical
revision as safety high-water.

For retrieval, core reconstructs the current reviewed port/grant/provider fence
from registry authority for each new admission (proposal bytes cannot mint it);
a response-lost dispatch may replay only its byte-identical operation before the
ordered safety invalidation.

Record retention refuses whole-installation selection; that belongs to the
multi-store purge coordinator: a server-issued preview plus exact durable
confirmation. It inventories every Storage Governance target, refuses while
memory/retrieval or lifecycle delivery (including the retained-uninstall event)
is unacknowledged, persists the preview before accepting confirmation, removes
installation-owned source and authority rows in one immediate registry
transaction, advances the installation to `purged`, and retains an exact
replayable 17-target receipt without recreating an outbox row the receipt proved
deleted. SQLite WAL, shared package/Artifact content, audit-policy rows and
provider history are disclosed as retained/unknown, not claimed erased. The
legacy candidate store is not the contribution destination owner.
