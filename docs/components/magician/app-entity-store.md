# App Entity Store

The app entity store extends the per-scope `apps/app_store.sqlite3`. There is
no database per app, no second scope-path resolver and no other connection
pool. `AppRegistryService` stays the only owner of paths, SQLite and blocking
admission.

Authenticated owner HTTP and a direct-personal-agent adapter sit over the same
service. Surfaces, directory, actions and the capability catalog are mounted
separately (see [app-platform contract kernel](app-platform-contract-kernel.md)).

## Schema compilation

The admitted manifest compiles into three correlated forms:

- an immutable package entity-schema digest over the entity declarations;
- an installation-effective validation schema whose field policies may only
  narrow the reviewed policy;
- a deterministic index plan covering only fields a view or query contract
  declares, plus reference fields needed for relation integrity.

Package identity is independent of any grant. The compiled schema embeds the
source entity-schema digest, which must match the package revision;
validation and canonical schemas must be byte-equivalent JSON. Query
contracts are rebuilt from the validated persisted documents; executable query
plans are never deserialized from a caller or trusted from storage.

## Storage ownership

The scoped registry schema holds append-only record revisions, record heads,
typed scalar indexes, text-search rows, mutation receipts, per-installation
change sequences, storage projections, a durable entity outbox, migration runs,
import receipts, crash-recoverable forget receipts, retention policies and run
journals, entity-change delivery leases, and query-cursor chains with one
immutable snapshot root. Completed forget receipts keep only selection kind and
digest, never raw record IDs. Purge and retention inventories include both
cursor types and materialized sort keys. Evidence rows carry no credentials,
grants, source scope IDs or package authority. The first read of an existing
scope runs any owed additive migration under the scope writer guard; later
reads stay read-only.

Constructing `AppEntityStoreService` touches no files, and reading an absent
scope creates nothing. Schema resolution uses one read-only connection and
fails closed unless principal/workspace, installation identity and generation,
active schema revision and package reference, compiled schema invariants and
the embedded digests all match exactly. Disabled, quarantined, retained,
purged, stale, corrupt or partial state never becomes readable. No Ollama,
vector, LLM, provider or network work happens here.

## Deterministic reads

`AppEntityStoreService::query` consumes a move-only store-authority fence,
rechecks scope, authentication, installation generation, package, grant and
schema revisions, and validates the request against a typed query schema
rebuilt from the compiled schema.

Scalar and NFKC text-search indexes update in the same transaction as the
dataset generation marker. Leaf equality/contains/prefix predicates use
indexes; other shapes use a capped scan. Either way each candidate is
re-evaluated, validated against the active schema and digest-checked. Text
comparison is case-sensitive, decimals compare exactly, and ordering ends with
the record-ID tie-breaker.

**Snapshot pagination** (default). Candidate read, generation check, cursor
lookup and page load share one SQLite snapshot. Clients get only an opaque
cursor reference. A cursor keeps its original 10-minute expiry
(`QUERY_CURSOR_TTL`) and names its child
deterministically, so a lost-response retry gets the same continuation. Only
the root, the current parent and its child are kept. Continuation fails closed
on request, order, schema or data drift, expiry, tamper, record change or
missing history. A later global dataset generation invalidates stale cursors.
Expired chains are cleaned in bounded batches.

Limits: one snapshot ≤ 10,000 revisions and 4 MiB; ≤ 256 cursors and 64 MiB
cursor evidence per installation; candidate reads and relation expansion each
≤ 64 MiB decoded.

Field projections carry value-schema identity, source revisions,
content/provenance/policy digests and the join of field and record policy.
Relation expansion is iterative, cycle-detecting, depth and row bounded, and
charged against one aggregate source-reference ceiling.

## Live keyset pagination

Queries may opt into `pagination: "keyset"` (supported-public contract 1.5.0);
omitting it keeps the snapshot contract. Keyset reads seek through current
heads and return at most the page plus one sentinel. Filter probes inspect at
most 101 index entries per conjunct. There is no 10,000-record ceiling;
payload and projection budgets still bound each response.

- The workflow `app_store_query` tool accepts the same mode. Native contextual
  rounds use keyset for recent-context queries and keep no cursor, so ambient
  reads cannot exhaust cursor quota. Reconciliation scans stay snapshots.
- A new installation returns an empty page before its first index-generation
  marker; imported records without the marker are never taken for an empty
  store.
- Typed ordering keys keep exact decimals, unsigned ints, UTC nanoseconds,
  text prefix order and null-last. A supported query has at most one indexed
  sort field plus record ID, and indexed equality/membership predicates under
  all/any. Anything else, or an incomplete index, returns
  `app_data_keyset_index_required`, never a silent full scan. Missing optional
  sort values form a final record-ID-ordered phase.
- The cursor stores ordering key, record ID, query evidence, installation
  binding and chain identity; each page rechecks authority and policy in one
  snapshot. A deleted or edited anchor does not break the next page; newer
  inserts arrive via the entity change stream. Keyset is not a historical
  export: sort-field edits can move rows across the cursor, and the live
  collection reconciles by ID.
- Each continuation renews a 24-hour idle expiry (a maximum, not a guarantee)
  and keeps only parent and newest child. At capacity, other chains are
  reclaimed (uncontinued roots first, then least recently advanced); the
  current chain is protected and records are never removed. An evicted
  reader gets the unavailable-cursor response and the SDK resets to page one.

`make test-app-indexed-query` checks the production SQL at scale against a
per-page SQLite VM-instruction budget.

## Optimistic mutations

`AppEntityStoreService::mutate` consumes a move-only mutation fence after the
same rechecks. The mutation identity is derived on the server from
installation, trusted origin and idempotency key, so no caller can swap the
command after authorization.

One SQLite transaction applies the whole create/update/delete/restore batch.
Existing records need exact revisions; new IDs are deterministic within the
mutation; payloads are revalidated; references are checked against the
in-flight overlay and durable heads. Deletes honor manifest `restrict` /
`nullify` / `cascade`, computed iteratively in one transaction. No recursive
walker exists.

The same commit appends revisions, advances heads and indexes, advances the
dataset generation, updates storage usage, reserves change sequences, stores
one mutation receipt and appends one projection event. Replay verifies
identity, digest, origin and sequence span. A conflicting replay, stale
evidence, storage excess, corruption or any limit rolls everything back.

Entity-change delivery is owned by `magician-apps/src/apps/entity_outbox.rs`: a
consumer claims ≤ 64 rows under a move-only lease of ≤ 5 minutes, then acks the
exact bytes or releases with a retry delay. Expired leases are recovered; stale
owners cannot ack or release. Background claims use the registry's reserved background
writer admission.

## Data portability

`AppDataPortabilityService` (`magician-apps/src/apps/entity_portability.rs`)
exports live records with user-scope authentication, not app authority, so
disabled, quarantined and uninstall-retained apps can return the owner's data
without running package code. Purged and review-only installations are
unavailable. The archive holds compatibility digests, payloads,
classifications and portable provenance; it excludes source IDs, grants,
credentials, schedules, memory and provider state.

Export and import-preview share a 10,000-record ceiling. Source identities
become archive aliases and references become portable tokens. Import derives
IDs from destination installation plus archive digest, so destinations never
share IDs. Exact package ID, bytes and schema are required; incompatible
migrations stay blocked.

Import first returns a preview (creates, conflicts, missing attachments,
compatibility). Approval binds preview, source digest, destination and
lifecycle generation. Commit re-resolves the destination, rewrites references,
joins source classification with the destination policy floor and writes in
one transaction. Conflicts are skipped only when unreferenced, never merged.
Exact replay returns the original receipt.

## Forget and retention

`AppEntityRetentionService` (`magician-apps/src/apps/entity_retention.rs`) uses
the data-owner resolver, so disabled or retained data stays deletable while
purged or corrupt bindings stay unavailable. Per-record forget computes
restrict/nullify/cascade effects before approval and inventories heads, all
revisions, indexes, projections and matching receipts; commit recomputes the
inventory.

SQLite `secure_delete` and a clean WAL boundary keep forgotten payloads out of
reusable pages and old frames. Deleted rows lose every revision and index;
nullified dependants are rewritten without old history, so the forgotten ID
cannot survive. State, a raw-ID-free rebuild event and a `pending_checkpoint`
receipt commit together; recovery finishes the checkpoint without repeating
deletion.

Retention policy activation is monotonic and idempotent. A run prunes only
non-head history by count, age and byte ceilings. WAL frames are truncated
before the receipt completes, and the receipt settles every examined item as
deleted or retained. Current heads are never candidates.

## Governed adapters

`AppEntityAdapterService` is the single data-plane adapter over
`AppEntityStoreService`; it owns no SQLite, cursors or admission lane. Owner
transport:

- `POST /api/magician/v2/apps/installations/{installation_id}/data/query`
- `POST /api/magician/v2/apps/installations/{installation_id}/data/mutations`

Both derive scope from middleware identity, require route and contract
installation to agree, admit bounded JSON, and mint a store fence from the
current installation/package/grant/schema tuple. Owner mutations use a
server-created `owner_api` origin tied to the session and idempotency key;
caller JSON cannot claim workflow or surface
provenance.

`AppGenericDataToolAdapter` is the query/search seam for a direct personal
agent. Each call consumes a fresh, non-cloneable authority minted from a
direct-owner execution and a trusted provider-registry grant; model input
cannot claim provider class or classification. The adapter narrows to the
grant's entity/field/search projection, and the store rejects ineligible
fields before scanning, so no consumer can widen
`none`/`local_only`/`remote_allowed`. The seam is not in the global
compiled-tool catalog, which cannot prove a direct-owner audience.

## Owner-approved age cleanup

The host owner can preview and delete records older than a date on any indexed
timestamp field. This is store maintenance: apps get no deletion capability
and no model runs. Jobs and candidate IDs are durable; there is no
10,000-record ceiling and no automatic expiry of app records.

- **Preview** freezes record IDs, revisions and generations without copying
  bodies. Totals describe the selection, not file shrinkage. Confirmation binds
  installation/schema/package generation, owner, scope, cutoff and selection
  digest, and expires after 15 minutes. A replacement preview releases the same
  owner's abandoned and expired selections; approved jobs are kept.
- **Advance** commits ≤ 64 records per batch. New or changed records are kept;
  reference effects outside the selection are kept and counted for review.
  Erasure uses the record-forget owner. Completion requires the secure-delete
  WAL checkpoint; the job then keeps only criteria, digest and outcome.
- **UI** runs batches in order; pausing stops after the current batch and
  reopening resumes, also after restart. Stopping keeps unprocessed records
  and still checkpoints already-erased data (retryable if busy). Deletion is permanent in this
  database; backups are not rewritten; Storage's reclaim operation shrinks the
  file.
- There is no ingestion cutoff: an importing app may recreate removed copies on
  a later sync (the UI says so before confirmation).
