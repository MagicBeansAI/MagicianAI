# App Platform Truth Baseline

**Owner:** app-platform integration; storage and lifecycle obligations stay with
the physical owners named below.

The living truth contract for Apps: status language, run and error semantics,
observability vocabulary and storage ownership. Implementation record:
completion ledger;
open operator evidence: `whattotest.md`. The
supported-public route inventory is generated from route metadata; the
2026-08-07 design
is archived.

## 1. Truthful status language

Every Apps claim uses the narrowest true state:

| Term | Required meaning |
| --- | --- |
| Contract or kernel present | Types, validators or internal owners exist; not reachability. |
| Production-wired | An authenticated product/API/CLI path reaches the owner; activated-path and release evidence may still be open. |
| User-reachable | A reviewed, enabled app reaches it through a supported surface. |
| Focused-verified | Named checks passed at the recorded revision and environment, nothing more. |
| Activated-path verified | authoring -> pack -> publish -> review -> enable -> discover -> dispatch -> settlement passed, with negative cases. |
| Release-qualified | The assembled revision passed compilation, regression, crash/replay, security, resource, compatibility and repeated-reliability gates. |
| Complete | Every core issue `verified` (or accepted `rejected` replacement), every core canary release-qualified, docs/contracts/routes/SDK/UI on that revision. |

Rules:

- A focused result covers only its own revision; later unexecuted edits do not
  inherit it. Implementation with unrun evidence is `in_progress`, not
  `verified`. "Implemented", "present", "wired" and "verified" are not synonyms.
- A generated TypeScript manifest artifact is not a transport SDK.
- Discovery or immutable locking does not imply dispatchability. Bounded dispatch covers
  only exact reviewed in-process, USR/CLI, Browser, macOS and Android owner
  action classes through their containment owners.
- Query, search, direct invocation, bounded three-hop composition and cursor
  subscriptions are production-wired; live two-app and assembled-product
  evidence is a release gate, not missing wiring.

Realtime voice has one production-wired lane: a directly authenticated,
identified personal-assistant session on one trusted, backend-proxied,
no-fallback profile. A server-only credential bound to scope, session, turn and
profile gates the catalog, dispatch, `local_only` memory rendering and provider
delivery; history and capture are off while it is active. Meetings,
hands-free, direct P2P, delegated/autonomous/feature workflows, public or
unknown owners, untrusted profiles and fallback fail closed.

## 3. Canonical run and result contract

Three distinct state layers:

1. `AppRunStatus`: the public logical-run lifecycle projected from Artifact V2,
   returned by run polling.
2. `AppActionStatus`: the settled result status in `AppActionResult`; not a
   second task state machine.
3. HTTP status: transport only. Clients never infer run terminality or effect
   settlement from it.

### 3.1 Artifact-to-run projection

Closed mapping owned by `app_run_status_from_task`:

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

`AppRunSnapshot.terminal` must equal `AppRunStatus::is_terminal()`. Polls
return the typed snapshot with HTTP 202 (nonterminal) or 200 (terminal); launch
acceptance returns 202; an HTTP error fabricates no run transition. `run_ref` is
the stable logical identity; a disclosed `execution_id` is an attempt
correlation and never replaces `run_ref` in retry, composition or control.
`task_id` is not in the public snapshot.

### 3.2 Result-state invariants

| `AppActionStatus` | Required result shape |
| --- | --- |
| `completed` | No error; typed output or at least one authoritative mutation/external-effect receipt. |
| `waiting` | No terminal error, output or committed-effect receipt. |
| `failed` | A typed error; no output or committed effect. |
| `uncertain` | `external_outcome_uncertain` + `outcome_uncertain` and an external-effect receipt. |

A terminal result matches the snapshot's `run_ref`, action and status.
`completed` without a typed result is corruption unless `result_withheld: true`
(only on `completed`, never with a serialized result) proves the result exists
and current policy withholds its bytes.

Cancellation is settlement, not a status rewrite: a requested cancel projects
`cancelling` until the owner proves `cancelled`, another terminal outcome or
`uncertain`. The supported-public cancel route (`CancelActionRun`,
`cancel_app_action_run_handler`) persists a durable intent and returns 200 only
when the receipt proves `cancelled`, else 202 with the settling receipt; an
already-terminal or not-started run, or a different cancellation request, is 409.

## 4. Canonical error mapping

`AppErrorEnvelope` is the Apps machine contract:

```text
code: stable AppErrorCode
disposition: stable AppErrorDisposition
message: bounded display context only
details: bounded, route-reviewed machine fields only
retry_after_ms?: positive delay
```

Only the disposition carries retry or effect semantics:

| Disposition | Client meaning |
| --- | --- |
| `terminal` | The request cannot progress without a semantic change. |
| `retry_same_input` | Retry only the exact request with the same idempotency key; the owner proved this safe. |
| `refresh_and_retry` | Refresh state and contracts, then submit a newly validated request. |
| `reauthorize` | Authentication, grant or provider authority is insufficient or stale. |
| `user_action_required` | An explicit reviewed user action is needed; polling or retry is not enough. |
| `outcome_uncertain` | An external effect may have crossed its boundary. Reconcile by stable run/receipt identity; never blind-retry with a new identity. |

Constraints:

- `external_outcome_uncertain` pairs only with `outcome_uncertain`, and vice
  versa.
- `retry_after_ms` (on `rate_limited` or retryable `unavailable`) does not prove
  a retry safe.
- `stale_revision` always pairs with `refresh_and_retry`; a conflicting
  immutable idempotency binding is `conflict` + `terminal`
  (`supported_public_error_semantics` in `magician-api/src/apps_api.rs`).
- `canceled` means proved no-effect or settled cancellation, never an uncertain
  effect.
- Internal errors expose only a stable code and generic message; storage,
  provider, policy-digest, filesystem, SQL, task, execution and authority
  details stay in restricted server logs/traces.
- Unauthorized or cross-scope existence may be concealed as `not_found`; error
  differences are not an installation or policy oracle.

| HTTP | Canonical error family |
| --- | --- |
| 400 | malformed `invalid_request` |
| 401/403 | `not_authorized` or concealed `not_found` |
| 404 | `not_found` |
| 409 | `conflict` or `stale_revision` |
| 410 | expired cursor/ephemeral evidence; refresh and retry |
| 413/422 | admitted size/schema/resource contract rejection |
| 429 | `rate_limited` or bounded-lane `resource_exhausted` |
| 500 | `internal`, no implementation detail |
| 503 | pre-effect `unavailable`; crossed-effect failures become `uncertain` |
| 504 | pre-effect `timeout`; crossed-effect timeouts become `uncertain` |

Supported-public inventory operations return `AppErrorEnvelope`; route-local
`{ "error", "message" }` on internal and non-inventoried routes is a legacy
adapter until migrated.

## 5. Payload-minimal trace vocabulary

Bounded stages, each emitted as one `tracing` event (target `magician.apps`,
message = the stage name) by `AppTraceEvent::emit`; workstreams may nest
physical-owner spans but add no synonymous top-level stages. A stage is emitted
only where its boundary exists; skipped work is not a successful event. Events
log at INFO, except a completed `effect_settlement` settle event, which is
DEBUG.

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

### 5.1 Trace/log attributes

The emitter writes exactly these 14 keys (`APP_TRACE_ATTRIBUTE_KEYS`); an
unset one is an empty string, meaning the boundary did not own that fact:

| Attribute | Type/values |
| --- | --- |
| `app.operation` | `contract_capabilities`, `query_data`, `mutate_data`, `launch_action`, `get_action_run`, `compose_action_run`, `cancel_action_run`, `read_entity_changes`, `workflow_launch`, `workflow_recovery`, `workflow_terminal`, `record_composition`, `action_result_composition`, `effect_settlement`, `worker_reconciliation` |
| `app.outcome` | `allowed`, `denied`, `completed`, `failed`, `cancelled`, `uncertain`, `unavailable` |
| `app.correlation_id`, `app.causation_id` | opaque `AppReference` |
| `app.action_id`, `app.workflow_id` | bounded `AppName` |
| `app.run_ref`, `app.child_id`, `app.resource_permit_ref`, `app.receipt_ref` | opaque `AppReference` |
| `app.mapping_digest` | `AppDigest` |
| `app.receipt_kind` | `effect`, `transfer`, `mutation` |
| `app.retry_class` | `none`, `same_input`, `refresh`, `reauthorize`, `user_action`, `reconcile_uncertain` |
| `app.effect_state` | `not_started`, `committed`, `settled_no_effect`, `uncertain` |

A unit test (`emitter_attribute_keys_are_the_payload_minimal_allowlist_subset`)
keeps payload, error-message, principal, workspace, provider, device, task-ID
and execution-ID keys out of that list. IDs, names, revisions and digests are
correlation fields, never metric labels.

### 5.2 Payload prohibition

Default traces, logs and metrics never contain record/action values, prompts,
model reasoning, tool arguments or results, memory candidates, rendered UI text,
DOM, screenshots, accessibility trees, Android views, clipboard, form values,
secrets, credentials, headers, cookies, tokens, raw URLs with
query/fragment/userinfo, filesystem paths, command lines, environment values,
SQL, provider responses or debug dumps; nor field names, descriptions, entity
payloads or dynamic content in span names, attribute keys, metric names or label
values. Payload capture needs a separate reviewed diagnostic owner with
retention and disclosure policy; general debug logs are not that owner.

### 5.3 Metric-label allowlist

Only these dimensions, each with a code-owned closed value set, may label Apps
metrics:

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

IDs of any kind, revisions, digests, scope hashes, paths, origins, domains,
bundle IDs, device IDs and error messages are prohibited labels.

### 5.4 Shared emitter and runtime adoption

`magician_v2::apps::observability` is the single Apps lifecycle-trace emitter.
Its builder has private fields and accepts only closed
stage/operation/outcome/retry/effect/receipt enums plus bounded `AppName`,
`AppReference` and `AppDigest` types, with no free-form, message, error, payload
or metric-label input and no metrics API; keys are limited to
`APP_TRACE_ATTRIBUTE_KEYS` and high-cardinality values stay trace fields. The
workflow owner (admitted launch, exact same-key recovery, launch/result and
terminal publication, effect settlement, bounded resource-worker
reconciliation), action-result composition (source run, destination action,
mapping digest and destination run as a child correlation, without mapped
values) and public HTTP adapters all emit through it, and none records
payloads, raw app data, raw errors, credentials, principal/workspace names,
provider/device/target details or internal task and execution IDs.

## 6. Durable storage-owner and migration map

Handlers and SDKs own no persistence; new durable state enters through a typed
port below, with lifecycle obligations settled first.

| Record/fact | Owner and representation | Migration/recovery | Export/retention/purge |
| --- | --- | --- | --- |
| Package, installation, attempt, grant, schema, approval, lifecycle outbox, directory metadata | `AppRegistryService`; scoped `apps/app_store.sqlite3` | Registry schema owner; additive versioned migration under the bounded per-scope writer and exact scope binding | Export excludes grants and scope; lifecycle owner handles retention/purge and outbox recovery |
| App entities, indexes, cursors, mutation/import/forget/retention receipts, entity outbox | `AppEntityStoreService` in the registry SQLite; schema v20+ (current registry v38) requires SQLCipher 4 pages, keyed by a domain-separated authenticated-scope key rooted in the OS keychain | Plaintext v1-v19 stores are verified, checkpointed, `sqlcipher_export`-rewritten, fsynced and atomically replaced; key-id metadata and retained root generations give crash-safe lazy rekey; historical migration validates the opening key before rekey to the active generation; missing, wrong or cross-scope keys fail closed | Typed export/import; retention/forget/purge owns physical cleanup and WAL. Row deletion is not per-install crypto-erasure (shared scope key); receipts reject that claim |
| Resource periods, trees, events, recovery evidence, workflow-control heads | `AppResourceAuthorityService` through registry-owned schema/transactions | Journal CAS plus typed recovery evidence; no second resource journal store | Resource owner settles bounded audit evidence and lifecycle cleanup |
| Source-linked app-memory candidate, source rows, disposable hybrid-index projection | `AppRegistryService` (`apps/memory_store.rs`, since registry schema v13) is candidate truth; accepted memory and the sealed projection under the scoped memory-index owner are destination-owned | Candidate publication/CAS and source settlement share registry transactions; destination delivery is atomic or deterministically reconstructed. Projection replacement precedes its content-free FullScope journal ack, so replay repairs a lost signal. RemoteAllowed rows persist; LocalOnly scores are credential-scoped and ephemeral | Source/lifecycle owners write an empty projection tombstone after commit, and whole-installation purge reaches the same idempotent seam; destination replay restores only currently accepted/live rows; no index row is export or authority |
| Staged immutable package bytes | `AppPackageStager`; scoped content-addressed `apps/packages/` | Descriptor-pinned write, fsync/create-only promotion, exact replay verification, bounded stale-staging recovery | Portability owner exports reviewed bytes; lifecycle deletes only exact unreferenced revisions |
| Data/combined portability archive | `AppDataPortabilityService` (logical records) + `portable_archive_transfer` (envelope); package ZIP via `package_transfer` | Default Argon2id-v19/XChaCha20-Poly1305 chunks authenticate the canonical header, chunk order, logical digest and exact combined package bytes; bounded preview/approve/transactional import; response-loss commit reopens the durable import receipt | Runtime-only passphrase, independent of SQLCipher keys; explicit plaintext is warned and denied for Secret data; foreign scope, install, grant, credential, schedule or memory authority never imported |
| Reviewed update/migration run, immutable staged record generation, backup and rollback receipts | `AppUpdateCoordinatorService` over the registry/entity/portability owners; since registry schema v21 it stores the exact run and hidden staged rows | Source-fence high-water, exact compiled operation bodies/digest, bounded dry-run, encrypted D2 backup receipt, staged tail catch-up, final active-generation CAS. Pre-switch abort deletes staged rows; response-loss replay reopens the same run/receipt. Code-only rollback creates a successor generation without rewriting data; data rewind replays the exact backup into the current schema and exposes conflict/new-local-ID decisions | Active reads never see staged rows; destructive migration requires the exact encrypted backup; rollback never restores prior grants, resurrects tombstoned memory/retrieval/index rows or overwrites conflicts; data rewind tombstones post-update live heads and keeps prior tombstones |
| App task manifest/state, workflow binding, terminal result, per-execution run state | Artifact V2 + `AppWorkflowService`: `tasks/<task>/...`, `state/app_workflow_binding.json`, `state/app_workflow_result.json`, execution-local `app_workflow_run.json` | Versioned typed documents, atomic write and journal recovery, canonical app namespace, task-record and start-admission locks | Artifact/task lifecycle owns archive/delete; run control keeps binding/result evidence until declared retention settles |
| Cross-app transfer identity and receipt | Sealed in the destination app-workflow binding | `AppWorkflowService` validates schema version, source/destination identity and deterministic replay; no standalone transfer database | Destination task retention; public/model projections omit internal receipt/provenance |
| Query/composition projection handles | Process-local bounded `AppProjectionHandleStore` | Non-durable; expiry, scope/session partition and restart rejection force re-query | Drop or expiry; never exported or recovery authority |
| Browser/macOS/Android session and observation handles | Physical dispatcher/host/device owner (future typed interactive port) | Opaque, session-local; restart requires re-observe and re-admit; durable outward effects settle into owner/workflow receipts | Cleanup on stop/revoke/update; sanitized bounded settlement evidence only |
| Recipe topology, node binding, child result/cancellation | V3/Artifact task owner via a versioned recipe binding | Deterministic compiler output and child bindings pinned before execution; replay from V3 events/receipts, no recipe database | Task lifecycle/export rules; no app-authored executable state |
| Contribution outbox for future destination ports | Source transaction owner (normally registry/entity/workflow settlement); destination reads via a typed repository port | Atomic outbox or deterministic rebuild from the source settlement | Source invalidates; destination owns derived retention and tombstone |
| Client/UI run cache | Non-authoritative sanitized cache keyed by authenticated scope and `run_ref` | Refetch on reload, gap or scope change; never repairs server state | Bounded eviction; cleared on logout or scope teardown; no bearer or payload |

Every new durable record declares schema owner, scope binding, atomicity,
idempotency, backup/restore, migration/rollback, crash recovery, export,
retention, purge/forget and reconciliation. A raw `rusqlite::Connection`,
filesystem path or ad hoc JSON file never crosses into an HTTP or compiled
handler.

### 6.1 Adjacent non-app lifecycle debt

Known limit: ordinary Artifact V2 status/delete/schedule writers do not all
take the root-start admission lock, so an ordinary task can race a lifecycle
mutation between commit and spawn (compare-and-commit still prevents a stale
root overwrite but does not serialize every generic writer). The fix belongs to
the Artifact V2 storage/lifecycle owner: share the root-start admission protocol
with those writers or prove an equivalent single serialization boundary.
Canonical app tasks already reject those generic writers and use their own typed start
permit, task-record lock, effect settlement and run control; the fix must not
widen generic APIs to app tasks or add a second app lifecycle protocol.

## 7. Activated-path canary assignment ledger

Package/canary assignment (C01–C16) is in the
completion ledger.
