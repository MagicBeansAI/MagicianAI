# App Entity Store

The app entity store extends the existing per-scope
`apps/app_store.sqlite3`. It does not create one database per app, a second
scope-path resolver or another connection pool. `AppRegistryService` remains
the sole path, SQLite ownership and bounded blocking-admission boundary.

Authenticated owner HTTP and a direct-personal-agent adapter sit over the
same service. Surfaces, directory, actions and the capability catalog are
mounted separately (see [app-platform contract kernel](app-platform-contract-kernel.md)).

## Schema compilation

The strict admitted manifest compiles into three correlated forms:

- an immutable package entity-schema digest over the canonical entity
  declarations;
- an installation-effective validation schema whose field policies may only
  narrow the reviewed policy;
- a deterministic index plan with only fields declared by a view/query
  contract, plus reference fields needed for bounded relation integrity.

Package identity is independent of any installation grant. The compiled schema
embeds the source entity-schema digest, which must match the immutable package
revision; validation and canonical schemas must be byte-equivalent JSON. Query
contracts are rebuilt from the validated persisted documents; executable query
plans are never deserialized from a caller or trusted from storage.

## Storage ownership

The scoped registry schema holds append-only record revisions, record heads,
typed scalar indexes, deterministic text-search rows, mutation receipts,
per-installation change sequences, storage/resource projections, a durable
entity outbox, resumable migration runs, data-import receipts,
crash-recoverable forget receipts, versioned retention policies and run
journals, entity-change delivery leases, and query-cursor chains with one
immutable snapshot root. Continuation cursors keep only bounded evidence and an
offset into the root; live keyset cursors keep one ordering boundary; completed
forget receipts keep only selection kind and digest, never raw record IDs.
Purge and retention inventories include both cursor types and materialized sort
keys. Evidence rows carry no credentials, grants, source scope IDs or package
authority. The first read of an existing scope performs any owed additive
migration under the per-scope writer guard, then records an in-process
readiness hint so steady-state reads stay read-only.

Constructing `AppEntityStoreService` has no filesystem effect; reading an absent
scope creates nothing. Active schema resolution uses one read-only connection
and fails closed unless all are exact: authenticated principal/workspace and
stored installation scope; enabled installation identity and lifecycle
generation; active schema revision and package reference; compiled schema
structure/index invariants; embedded source schema digest and package revision
digest. Disabled, quarantined, retained, purged, stale, corrupt or partial
state never becomes readable app data. No Ollama, vector, LLM, provider or
network work happens on this boundary.

## Deterministic reads

`AppEntityStoreService::query` consumes a move-only store-authority fence and
rechecks live scope binding, authentication revision, installation generation,
package, grant and schema revisions, then validates the request against a typed
query schema rebuilt from the persisted compiled schema.

Scalar and normalized text-search indexes are maintained in the same transaction
as the dataset generation marker. Leaf equality/contains/prefix predicates use
indexes; compound or unsupported ones use a capped deterministic scan. Either
way the service re-evaluates the typed predicate per candidate, validates every
payload against the active schema, and verifies payload and effective-policy
digests. Text comparison is NFKC and case-sensitive; decimals compare exactly;
ordering always ends with the stable record-ID tie-breaker.

**Snapshot pagination** (default). Candidate read, generation check, cursor
lookup and revision-page load share one SQLite read snapshot. Pagination
persists a compact revision list and non-deserializable cursor evidence in the
scoped owner; clients get only an opaque cursor reference. A cursor keeps its
original issue/expiry window and deterministically names its child, so a
lost-response retry of the same parent gets the same continuation. After a
later child is consumed, only the snapshot root, current replayable parent and
its child are kept. Continuation re-mints and verifies evidence and fails closed
on request/order/schema/data drift, expiry, tamper, record update/deletion or
unavailable history. Expired chains are cleaned in bounded batches.

Limits: one snapshot ≤ 10,000 revisions and 4 MiB (versioned iterative binary
codec, stored once per chain); ≤ 256 cursors and 64 MiB cursor evidence per
installation; candidate reads and relation expansion each ≤ 64 MiB decoded.
Cursor pages load revisions in one bounded query.

Field projections carry content-addressed value-schema identity, exact source
revisions, content/provenance/policy digests and the join of selected field
policy with every returned record policy. Relation expansion is iterative,
cycle-detecting, depth/row bounded and charged against one aggregate
source-reference ceiling before it can multiply reads.

## Live keyset pagination

Queries may opt into `pagination: "keyset"` (supported-public contract 1.5.0);
omitting it keeps the frozen snapshot contract. Keyset reads seek through
current heads, returning at most the page plus one metadata sentinel. Selective
filter probes inspect at most 101 index entries per conjunct. There is no
10,000-record collection ceiling and no archive/truncation past that size;
payload and projection budgets still bound each response.

- The workflow `app_store_query` tool accepts the same mode. Native contextual
  rounds choose keyset for supported recent-context queries before binding
  resource permits and retain no continuation cursor, so ambient reads cannot
  exhaust cursor quota. Frozen reconciliation scans stay explicit snapshots.
- An empty new installation returns an empty page before its first
  index-generation marker; imported records without the marker are never
  mistaken for an empty store.
- Typed ascending/descending ordering keys (backfilled from scalar indexes,
  maintained atomically by mutations) preserve exact decimals, unsigned ints,
  UTC nanoseconds, text prefix order and null-last. Supported queries have at
  most one indexed sort field (plus implicit ascending record ID) and indexed
  equality/membership predicates combined with all/any. Unsupported shapes or
  incomplete indexes return `app_data_keyset_index_required` — never a silent
  full scan. Missing optional sort values form a final record-ID-ordered phase.
- The opaque cursor stores ordering key + record ID, query evidence,
  installation/package/schema binding and chain identity; each page rechecks
  authority and policy in one snapshot. A deleted/edited anchor does not break
  the next page; newer inserts arrive via the entity change stream. A keyset
  traversal is not a historical export: sort-field edits can move rows across
  the cursor, and the live collection reconciles by ID.
- Each continuation renews a 24-hour idle expiry (a maximum, not a guarantee)
  and keeps only parent and newest child. At capacity, insertion reclaims other
  chains in the installation (uncontinued roots first, then least recently
  advanced); the current chain is protected and records are never removed. An
  evicted reader gets the unavailable-cursor response; the SDK live collection
  resets to the first page.

`make test-app-indexed-query` checks the production SQL at scale with a
per-page SQLite VM-instruction budget.

## Optimistic mutations

`AppEntityStoreService::mutate` consumes a move-only mutation fence only after
rechecking scope, authentication, installation generation, package, grant and
schema revisions. The logical mutation identity is derived server-side from
installation, trusted origin and protocol idempotency key; callers cannot
substitute a command after authorization.

One registry-owned SQLite transaction applies the whole create/update/delete/
restore/scalar-reference batch. Existing records and relation endpoints need
exact revisions; new record IDs are deterministic within the mutation identity;
payloads are revalidated; every live reference target is checked against the
in-flight overlay and durable heads. Reference deletion honors manifest
`restrict` / `nullify` / `cascade`: cascade closure is computed iteratively,
restrict decisions deferred until closure is known, nullification applied in
the same transaction. Denied-cycle validation is iterative under one aggregate
budget. No recursive walker exists.

The same commit appends revisions, advances heads, replaces scalar/text
indexes, advances the global dataset generation, updates storage usage,
reserves monotonic change sequences, stores one canonical mutation receipt and
appends one pending projection event. Receipt replay verifies identity, digest,
origin and sequence span. Conflicting replay, stale evidence, storage excess,
corruption or any expansion limit rolls everything back. Global generation
invalidates stale cursors; untouched heads keep their own generation.

Entity-change delivery is owned by `magician-apps/src/apps/entity_outbox.rs`: a
consumer claims ≤ 64 due rows under a private move-only owner/token lease of
≤ 5 minutes, then acks the exact stored bytes or releases with bounded retry
delay. Later claims recover expired leases; stale owners cannot ack or release.
Background claims use the registry's reserved background writer admission.

## Data portability

`AppDataPortabilityService` (`magician-apps/src/apps/entity_portability.rs`)
exports current live records with user-scope authentication, not app authority,
so disabled, quarantined and uninstall-retained apps can return the owner's
data without running package code. Purged and review-only installations are
unavailable. The archive holds package/schema compatibility digests, canonical
payloads, effective classifications and portable provenance; it excludes source
scope/installation IDs, grants, credentials, schedules, memory and provider
state.

Export and import-preview share a 10,000-record ceiling; payloads are bounded
per record and per archive, and export moves decoded values rather than cloning
the dataset. Source record/actor/execution identities become typed archive
aliases; references become portable tokens revalidated against the archive
graph. Import derives identities from destination installation + archive
digest, so destinations never share imported IDs. Exact package ID, bytes and
schema identity are required; incompatible migrations stay blocked.

Import first yields a canonical preview (creates, conflicts, missing
attachments, compatibility). A serialization-only approval binds preview,
source digest, destination installation and lifecycle generation. Commit
re-resolves the destination, recomputes IDs, rewrites references, joins source
classification with the destination policy floor, checks relations/cycles and
writes everything in one transaction. Conflicts are skipped only when
unreferenced, never merged. Exact replay returns the original receipt.

## Forget and retention

`AppEntityRetentionService` (`magician-apps/src/apps/entity_retention.rs`) uses
the data-owner schema resolver, so disabled or retained data stays deletable
while purged/corrupt bindings stay unavailable. Per-record forget computes
restrict/nullify/cascade effects before approval and inventories affected heads,
all historical revisions, indexes, query projections and matching
mutation/outbox evidence; approval is revalidated before destruction and commit
recomputes the inventory in-transaction.

SQLite `secure_delete` and a clean WAL boundary keep forgotten payloads out of
reusable pages and old frames. Deleted and cascade rows lose every revision and
index; nullified dependants lose old history and are rewritten, so the forgotten
ID cannot survive in append-only history. Dataset/storage/sequence state, a
raw-ID-free rebuild event and a `pending_checkpoint` receipt commit together;
recovery finishes the checkpoint without repeating deletion. Auxiliary evidence
cleanup matches typed entity/record pairs, not arbitrary JSON strings.

Retention policy activation is monotonic and idempotent. A run prunes only
non-head history by per-record count, age and aggregate byte ceilings using SQL
windows. The pending journal commits with pruning; post-commit WAL frames are
measured and truncated before the receipt completes. The receipt binds scope,
installation and policy and settles every examined logical/WAL item as deleted
or retained. Current heads are never candidates.

## Governed adapters

`AppEntityAdapterService` is the single data-plane adapter over
`AppEntityStoreService`; it owns no SQLite, cursors, mutation semantics or
admission lane. Owner transport:

- `POST /api/magician/v2/apps/installations/{installation_id}/data/query`
- `POST /api/magician/v2/apps/installations/{installation_id}/data/mutations`

Both derive scope from middleware identity, require route and contract
installation to agree, decode bounded JSON only after byte/depth/node admission,
and mint a move-only store fence from the current installation/package/grant/
schema tuple. Owner mutations use a server-created `owner_api` origin tied to the
session and idempotency key; caller JSON cannot claim workflow or surface
provenance.

`AppGenericDataToolAdapter` is the query/search seam for a direct personal
agent. Each call consumes a fresh non-cloneable, non-deserializable authority
minted from a resolved direct-owner execution and a trusted provider-registry
grant; model input cannot claim provider class or maximum classification. The
adapter intersects the grant's entity/field/search projection before store
access; the store rejects an ineligible field policy before scanning, joins
narrower record policy, and rechecks before publishing a cursor, so no consumer
can widen `none`/`local_only`/`remote_allowed`. The seam is not in the global
compiled-tool catalog, which cannot prove a direct-owner audience and provider
attestation.

## Owner-approved age cleanup

The host owner can preview and delete records older than a chosen date on any
indexed timestamp field. This is entity-store maintenance: apps get no deletion
capability and no model runs. Cleanup jobs and candidate identities are durable
and disk-backed; there is no 10,000-record selection ceiling and no automatic
expiry of app records.

- **Preview** uses the timestamp order index and freezes record IDs, revisions
  and dataset generations without copying bodies. Count and byte totals describe
  the selection, not file shrinkage. Confirmation binds
  installation/schema/package generation, owner, scope, cutoff and selection
  digest and expires after 15 minutes. A replacement preview releases the same
  owner's abandoned and expired selections; approved jobs are kept.
- **Advance** commits ≤ 64 records plus progress per batch. New or changed
  records are kept; reference effects outside the frozen selection or beyond a
  batch are kept and counted for review. Erasure uses the record-forget owner
  (revisions, indexes, affected receipts and projections). Completion requires
  the secure-delete/WAL checkpoint; temporary candidate IDs are then removed and
  the job keeps criteria, digest and aggregate outcome.
- **UI** drives batches sequentially; pause/close stops after the in-flight
  batch and reopening continues from durable progress (also after restart).
  Stopping keeps unprocessed records and still checkpoints already-erased data
  (retryable if busy). Deletion is permanent in this database; backups are not
  rewritten; Storage's reclaim operation separately shrinks the file.
- It imposes no ingestion cutoff: an importing app may recreate removed copies
  on a later sync (the UI says so before confirmation).
