# App Platform Truth Baseline

**Owner:** app-platform integration, with storage and lifecycle obligations
retained by the named physical owners below.

This document is the attributable living truth contract for Apps. It fixes the
status language, run/error semantics, observability vocabulary, and storage
ownership. The completed implementation record is archived in the
completion ledger;
remaining operator evidence is maintained in
`whattotest.md`.

The supported-public route inventory is generated from route metadata, not kept here.

## 1. Truthful status language

Every Apps claim uses the narrowest applicable state:

| Term | Required meaning |
| --- | --- |
| Contract or kernel present | Types, validators, reducers, or internal owners exist. This says nothing about product reachability. |
| Production-wired | An authenticated product/API/CLI path reaches the owner, but its activated-path and release evidence may remain open. |
| User-reachable | A real reviewed and enabled app can reach the path through a supported surface. |
| Focused-verified | The named focused checks passed at the recorded revision and environment. It does not imply later edits or the whole platform passed. |
| Activated-path verified | The complete authoring -> pack -> publish -> review -> enable -> discover -> dispatch -> settlement path passed, including its negative cases. |
| Release-qualified | The assembled revision passed owned compilation, regression, crash/replay, security, resource, compatibility, and repeated-reliability gates. |
| Complete | Every core issue is `verified` or has an accepted `rejected` replacement, every core canary is release-qualified, and docs/contracts/routes/SDK/UI describe that same revision. |

Rules:

- A historical focused result remains evidence for that historical revision;
  later unexecuted edits do not inherit it.
- “Implemented,” “present,” “wired,” and “verified” are not synonyms.
- A generated TypeScript manifest artifact is not a transport SDK.
- Discovery or immutable locking does not imply dispatchability. Bounded
  dispatch covers only exact reviewed in-process, USR/CLI, Browser, macOS, and
  Android owner action classes through their named containment owners.
- Query/search/direct invocation plus bounded three-hop composition and cursor
  subscriptions are production-wired. Live two-app and assembled-product
  evidence remains a release gate, not missing product wiring.
- A work package with implementation and unrun evidence is `in_progress`, not
  `verified`.

Realtime voice has one deliberately narrow production-wired owner lane: an
identified, directly authenticated personal-assistant session using one
explicitly trusted backend-proxied, no-fallback realtime profile. Its
server-only credential is exact-scope/session/turn/profile bound and gates
governed catalog, dispatch, `local_only` memory rendering, and final provider
delivery. History/capture is disabled while it is active. Meetings,
hands-free, direct P2P, delegated/autonomous/feature workflows, public/unknown
owners, untrusted profiles, and fallback remain fail-closed.

The archived
2026-08-07 design
records the completed program. This baseline owns the living meaning of its
terms.

## 3. Canonical run and result contract

Apps have three deliberately distinct state layers:

1. `AppRunStatus` is the public logical-run lifecycle projected from Artifact
   V2. It is the status returned by run polling.
2. `AppActionStatus` is the settled typed result status carried by
   `AppActionResult`; it is not a second task state machine.
3. HTTP status reports transport handling. Clients never infer run terminality
   or effect settlement from HTTP status alone.

### 3.1 Artifact-to-run projection

The closed mapping owned by `app_run_status_from_task` is:

| Artifact task status | `AppRunStatus` | Terminal |
| --- | --- | --- |
| `pending`, `ready`, `queued`, `starting` | `queued` | no |
| `planning` | `planning` | no |
| `running` | `running` | no |
| `paused` | `paused` | no |
| `deferred` | `deferred` | no |
| `waiting` or `waiting_*` | `waiting` | no |
| `blocked` | `blocked` | no |
| `cancelling`, `cancel_requested`, `cancellation_requested` | `cancelling` | no |
| `completed` | `completed` | yes |
| `failed` | `failed` | yes |
| `cancelled`, `canceled` | `cancelled` | yes |
| `archived` | `archived` | yes |
| `uncertain` | `uncertain` | yes |
| unknown value | corruption/internal error | n/a |

`AppRunSnapshot.terminal` must equal `AppRunStatus::is_terminal()`. A
nonterminal poll returns HTTP 202 with the typed snapshot; a terminal poll
returns HTTP 200 with the typed snapshot. Launch acceptance returns HTTP 202.
An HTTP error returns no fabricated run transition.

`run_ref` is the stable logical identity. `execution_id`, when disclosed, is
an attempt/correlation value and must not replace `run_ref` in retry,
composition, or control. `task_id` is not part of the public run snapshot.

### 3.2 Result-state invariants

| `AppActionStatus` | Required result shape |
| --- | --- |
| `completed` | No error. Requires typed output or at least one authoritative mutation/external-effect receipt. |
| `waiting` | No terminal error, output, or committed-effect receipt. |
| `failed` | Requires a typed error. It cannot claim output or a committed effect. |
| `uncertain` | Requires `external_outcome_uncertain` + `outcome_uncertain` and an external-effect receipt. |

A terminal result must match the snapshot's `run_ref`, action, and projected
status. `completed` without a typed result is corruption unless
`result_withheld: true` proves the result exists and current policy withholds
its bytes. `result_withheld` is valid only for `completed` and never coexists
with a serialized result.

Cancellation is truthful settlement, not an eager status rewrite. A requested
cancel projects `cancelling` until the owner proves `cancelled`, another
terminal outcome, or `uncertain`. The supported-public cancel route
(`CancelActionRun`, `cancel_app_action_run_handler`) persists a durable
cancellation intent and returns HTTP 200 only when the receipt proves
`cancelled`, otherwise HTTP 202 with the still-settling receipt; an
already-terminal or not-started run, or a different cancellation request, is
HTTP 409.

## 4. Canonical error mapping

`AppErrorEnvelope` is the Apps machine contract:

```text
code: stable AppErrorCode
disposition: stable AppErrorDisposition
message: bounded display context only
details: bounded, route-reviewed machine fields only
retry_after_ms?: positive delay
```

The message and HTTP status are never retry or effect semantics. The
disposition controls client behavior:

| Disposition | Client meaning |
| --- | --- |
| `terminal` | The same request cannot make progress without a semantic change. |
| `retry_same_input` | Retry only the exact request with the same idempotency key. The owner has proved that advice is safe. |
| `refresh_and_retry` | Refresh canonical state/contracts, then submit a newly validated request. |
| `reauthorize` | Current authentication/grant/provider authority is insufficient or stale. |
| `user_action_required` | The owner needs an explicit reviewed user action; polling/retry alone is insufficient. |
| `outcome_uncertain` | An external effect may have crossed its boundary. Reconcile by stable run/receipt identity; never blind-retry with a new identity. |

Mandatory pairings and constraints:

- `external_outcome_uncertain` pairs only with `outcome_uncertain`, and every
  `outcome_uncertain` error uses that code.
- `rate_limited` and a retryable `unavailable` may carry a positive
  `retry_after_ms`; the field is not accepted as proof that a retry is safe.
- `stale_revision` normally uses `refresh_and_retry`; a conflicting immutable
  idempotency binding is `conflict` + `terminal`.
- `canceled` describes a proved no-effect or settled cancellation. It cannot
  hide an uncertain effect.
- Internal errors expose a stable code and generic display message. Exact
  storage, provider, policy digest, filesystem, SQL, task, execution, or
  authority details remain in restricted server logs/traces.
- Concealment may project unauthorized or cross-scope existence as
  `not_found`; clients must not use error differences as an installation or
  policy oracle.

Canonical transport classes are:

| HTTP | Canonical error family |
| --- | --- |
| 400 | malformed `invalid_request` |
| 401/403 | `not_authorized` or concealed `not_found` |
| 404 | `not_found` |
| 409 | `conflict` or `stale_revision` |
| 410 | expired cursor/ephemeral evidence; refresh and retry |
| 413/422 | admitted size/schema/resource contract rejection |
| 429 | `rate_limited` or bounded-lane `resource_exhausted` |
| 500 | `internal`, with no implementation detail |
| 503 | pre-effect `unavailable`; crossed-effect failures become run/result `uncertain` |
| 504 | pre-effect `timeout`; crossed-effect timeouts become run/result `uncertain` |

Existing route-local `{ "error", "message" }` responses remain legacy adapters,
not a second canonical contract. The supported-public inventory operations use
`AppErrorEnvelope` at their route boundary; internal and non-inventoried routes retain their legacy
compatibility shape until separately migrated.

## 5. Payload-minimal trace vocabulary

Instrumentation uses the following bounded span stages. Workstreams may nest
physical-owner spans, but must not invent synonymous top-level stage names.

```text
app.request
  -> app.resolve
  -> app.policy
  -> app.admit
  -> app.execute
  -> app.effect
  -> app.settle
  -> app.publish
  -> app.reconcile
```

Not every operation emits every stage. A stage is emitted only when that
boundary exists; skipped work is not represented as a successful span.

### 5.1 Trace/log attributes

| Attribute | Type/values | Boundary |
| --- | --- | --- |
| `app.operation` | stable bounded operation name | every span |
| `app.outcome` | `allowed`, `denied`, `completed`, `failed`, `cancelled`, `uncertain`, `unavailable` | terminal stage |
| `app.correlation_id`, `app.causation_id` | opaque IDs | request/child/event chains |
| `app.scope_binding_hash` | domain-separated digest | authenticated resolution |
| `app.installation_generation` | positive integer | installation resolution |
| `app.package_revision`, `app.schema_revision`, `app.grant_revision` | opaque revision/digest | resolution/revalidation |
| `app.primitive_kind`, `app.primitive_revision`, `app.primitive_digest` | closed kind plus immutable identity | primitive resolution |
| `app.execution_class`, `app.containment_class` | reviewed closed enums | admission/execution |
| `app.action_id`, `app.workflow_id`, `app.recipe_node_id` | bounded IDs | logical execution |
| `app.run_ref`, `app.task_id`, `app.execution_id`, `app.child_id` | opaque IDs | durable correlation |
| `app.interactive_profile`, `app.session_ref`, `app.observation_sequence`, `app.observation_digest` | closed profile/opaque evidence | interactive owner |
| `app.target_policy` | `allowed`, `denied`, `protected`, `stale` | interactive/network target gate |
| `app.policy_decision`, `app.denial_code` | closed decision/stable code | policy boundary |
| `app.input_bytes`, `app.output_bytes` | nonnegative integer | admitted encoded bytes |
| `app.input_classification`, `app.output_classification` | closed handling labels | disclosure boundary |
| `app.accepted_field_count`, `app.refused_field_count`, `app.mapping_digest` | counts/digest | mapping |
| `app.resource_permit_ref`, `app.resource_reserved`, `app.resource_actual`, `app.resource_overrun` | opaque ref/bounded numeric summaries | resource owner |
| `app.receipt_kind`, `app.receipt_ref` | closed kind/opaque ref | mutation/transfer/effect/proposal settlement |
| `app.retry_class` | `none`, `same_input`, `refresh`, `reauthorize`, `user_action`, `reconcile_uncertain` | terminal stage |
| `app.effect_state` | `not_started`, `committed`, `settled_no_effect`, `uncertain` | effect/settlement |
| `app.cancellation_state` | `none`, `requested`, `settled`, `too_late` | control boundary |
| `app.error_code` | stable bounded code | failure only |
| `app.latency_ms` | nonnegative integer | completed span |
| `app.outbox_state`, `app.outbox_age_ms`, `app.outbox_attempt` | closed state/numeric | outbox/reconciliation |

IDs, names, revisions, and digests are trace/log correlation fields, never
metric labels. Raw principal/workspace strings are not recorded; only the
domain-separated scope-binding hash may be used.

### 5.2 Payload prohibition

Default traces, logs, and metrics must not contain:

- record/action input or output values, prompts, model reasoning, tool
  arguments/results, memory candidates, or rendered UI text;
- DOM, screenshots, accessibility trees, Android views, clipboard contents,
  form values, secrets, credentials, headers, cookies, or tokens;
- raw URLs with query/fragment/userinfo, filesystem paths, command lines,
  environment values, SQL, provider responses, or internal error/debug dumps;
- field names, app/user descriptions, entity payloads, or dynamic content in
  span names, attribute keys, metric names, or label values.

Payload capture requires a separate explicit, reviewed diagnostic owner with
retention and disclosure policy. Enabling general debug logs is not that owner.

### 5.3 Metric-label allowlist

Only these dimensions may label Apps metrics:

```text
operation
outcome
error_class
execution_class
containment_class
policy_decision
effect_state
retry_class
```

Each label uses a code-owned closed value set. Installation/package/schema/
grant/primitive/action/workflow/recipe/run/task/execution/session/observation/
receipt IDs, revisions, digests, scope hashes, paths, origins, domains, bundle
IDs, device IDs, and error messages are prohibited metric labels.

### 5.4 Shared emitter and runtime adoption

`magician_v2::apps::observability` is the single Apps lifecycle-trace emitter. Its
builder has private fields and accepts only closed stage/operation/outcome/
retry/effect/receipt enums plus bounded `AppName`, `AppReference`, and
`AppDigest` contract types. It has no free-form field, message, error, payload,
or metric-label input. The exact attribute-key subset is code-owned by
`APP_TRACE_ATTRIBUTE_KEYS`; high-cardinality values remain trace fields and
the module exposes no metrics API.

The common workflow owner emits admitted launch, exact same-key recovery,
launch/result publication, terminal publication, effect settlement, and
bounded resource-worker reconciliation events. Action-result composition adds
the source run, destination action, mapping digest, and destination run as a
child correlation without logging mapped values. Public HTTP adapters use the
same emitter and operation vocabulary at request/outcome boundaries. None of
these paths records payloads, raw app data, raw errors, credentials,
principal/workspace names, provider/device/target details, or internal task and
execution identifiers.

## 6. Durable storage-owner and migration map

Handlers and SDKs own no persistence. New durable state must enter through the
typed port in this table, with its lifecycle obligations settled before a
representation lands.

| Record/fact | Authoritative owner and representation | Migration/recovery | Export/retention/purge |
| --- | --- | --- | --- |
| Package, installation, attempt, grant, schema, approval, lifecycle outbox, directory metadata | `AppRegistryService`; scoped `apps/app_store.sqlite3` | Registry schema owner; additive/versioned migration under the bounded per-scope writer and exact scope binding | Package export excludes grants/scope; lifecycle owner handles retention/purge and outbox recovery |
| App entities, indexes, cursors, mutation/import/forget/retention receipts, entity outbox | `AppEntityStoreService` through the same registry SQLite owner; schema v20 requires SQLCipher 4 pages keyed by a domain-separated authenticated-scope key rooted in the OS keychain | Plaintext v1-v19 stores are verified, checkpointed, `sqlcipher_export`-rewritten, fsynced and atomically replaced; key-id metadata and retained root generations support crash-safe lazy rekey. Historical schema migration validates the key that opened the store before the caller rekeys it to the active generation; missing/wrong/cross-scope keys fail closed | Typed data export/import; entity retention/forget/purge owns physical cleanup and WAL obligations. The shared scope key means installation row deletion is not per-install crypto-erasure, and the receipt contract rejects that claim |
| Resource periods, trees, events, recovery evidence, workflow-control heads | `AppResourceAuthorityService` through registry-owned schema/transactions | Journal CAS plus typed recovery evidence; no second resource journal store | Resource owner settles/retains bounded audit evidence and lifecycle cleanup |
| Source-linked app-memory candidate, source rows and disposable hybrid-index projection | `AppRegistryService` (`apps/memory_store.rs`) in registry schema v13 is candidate truth; canonical accepted memory and the sealed projection under the scoped memory-index owner remain destination-owned | Candidate publication/CAS and source settlement share registry transactions; destination delivery is atomic or deterministically reconstructed. Projection replacement precedes its content-free FullScope journal acknowledgement, so replay repairs a lost post-commit signal. RemoteAllowed rows persist; LocalOnly scores are credential-scoped and ephemeral | Source/lifecycle owners write an empty projection tombstone after commit; destination replay may restore only currently accepted/live rows. Whole-installation purge reaches the same idempotent tombstone seam; no index row is export or authority |
| Staged immutable package bytes | `AppPackageStager`; scoped content-addressed `apps/packages/` | Descriptor-pinned write, fsync/create-only promotion, exact replay verification, bounded stale-staging recovery | Package portability owner exports reviewed bytes; lifecycle/retention deletes only exact unreferenced revisions |
| Data/combined portability archive | `AppDataPortabilityService` for logical record projection plus `portable_archive_transfer` for the physical envelope; exact package ZIP remains owned by `package_transfer` | Default Argon2id-v19/XChaCha20-Poly1305 chunks authenticate the canonical header, chunk order, logical digest and exact combined package bytes. Import is bounded preview/approve/transactional commit; response-loss commit reopens the durable entity import receipt | Archive passphrase is runtime-only and independent of SQLCipher scope keys. Explicit plaintext is warned and denied for Secret data; foreign scope/install/grant/credential/schedule/memory authority is never imported |
| Reviewed update/migration run, immutable staged record generation, backup and rollback receipts | `AppUpdateCoordinatorService` over the existing registry/entity/portability owners; registry schema v21 stores the exact run and hidden staged rows | Source-fence high-water, exact compiled operation bodies/digest, bounded dry-run, encrypted D2 backup receipt, staged tail catch-up and final active-generation CAS share registry lifecycle truth. Pre-switch abort deletes staged rows; response-loss replay reopens the same run/receipt. Code-only rollback creates a successor lifecycle generation without rewriting data; reviewed data rewind replays the exact backup into current schema and exposes conflict/new-local-ID decisions | Active reads never use staged rows. Destructive migration requires the exact encrypted backup. Rollback never restores prior grants, resurrects tombstoned memory/retrieval/index rows, or overwrites conflicts; data rewind tombstones post-update live heads and preserves prior tombstones |
| App task manifest/state, workflow binding, terminal result, per-execution run state | Artifact V2 workspace plus `AppWorkflowService`; `tasks/<task>/...`, `state/app_workflow_binding.json`, `state/app_workflow_result.json`, and execution-local `app_workflow_run.json` | Versioned typed documents, Artifact atomic write/journal recovery, canonical app namespace, task-record and start-admission locks | Artifact/task lifecycle owns archive/delete; typed app run control preserves binding/result evidence until its declared retention settles |
| Cross-app transfer identity and receipt | Sealed inside the destination Artifact app-workflow binding | `AppWorkflowService` validates schema version, source/destination identity, and deterministic replay; no standalone transfer database | Follows destination task retention; public/model projections omit internal receipt/provenance |
| Query/composition projection handles | Process-local bounded `AppProjectionHandleStore` | Deliberately non-durable; expiry, scope/session partition, and restart rejection force authoritative re-query | Drop/expiry only; never exported or treated as recovery authority |
| Browser/macOS/Android session and observation handles | Existing physical dispatcher/host/device owner through the future typed interactive port | Deliberately opaque and normally process/session-local; restart requires re-observe/re-admit. Durable outward effects settle into owner/workflow receipts | Owner cleanup on stop/revoke/update; only sanitized bounded audit/settlement evidence is retained |
| Recipe topology, node binding, child result/cancellation | V3/Artifact task owner through a versioned recipe binding | Deterministic compiler output and child bindings are pinned before execution; replay uses V3 events/receipts, not a recipe database | Task lifecycle/export rules; no arbitrary app-authored executable state |
| Contribution outbox for future destination ports | Source transaction owner, normally registry/entity/workflow settlement; destination consumes through a typed repository port | Atomic outbox where possible, otherwise deterministic reconstruction from the authoritative source settlement | Source lifecycle emits invalidation; destination owns derived retention and tombstone |
| Client/UI run cache | Non-authoritative sanitized client cache keyed by authenticated scope and `run_ref` | Refetch after reload, gap, or scope change; never repairs server state | Bounded eviction/logout/scope teardown; no bearer or payload retention |

Every future durable record must declare: schema/version owner, scope binding,
atomicity boundary, idempotency identity, backup/restore, migration/rollback,
crash recovery, export, retention, purge/forget, and reconciliation. A raw
`rusqlite::Connection`, filesystem path, or ad hoc JSON file may not cross into
an HTTP/compiled handler to satisfy that obligation.

### 6.1 Adjacent non-app lifecycle debt

Ordinary Artifact V2 status/delete/schedule writers do not all share the
root-start admission lock. An ordinary task can therefore race a lifecycle
mutation in the commit-to-spawn interval. The fresh reducer compare-and-commit
prevents a stale root overwrite, but does not by itself serialize every generic
writer.

This is non-app debt owned by the Artifact V2 storage/lifecycle owner: the fix
is to share the root-start admission protocol with those generic writers or prove
an equivalent single serialization boundary.

Canonical app task IDs already reject those generic lifecycle surfaces and use
their own typed start permit, task-record lock, effect settlement, and run
control. The ordinary-task repair must preserve that fence; it must not widen
generic status/delete/schedule APIs to canonical app tasks or introduce a
second app lifecycle protocol.

## 7. Activated-path canary assignment ledger

Package/canary assignment (C01–C16) is in the
completion ledger.
