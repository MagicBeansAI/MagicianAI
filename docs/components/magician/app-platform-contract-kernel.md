# App Platform Contract Kernel

Living contract for the app-platform kernel: types, owners, routes and invariants. Status
vocabulary, Artifact-to-app run mapping, canonical error dispositions, payload-minimal trace
fields, storage-owner obligations and canary assignments live in the
[App Platform truth baseline](app-platform-truth-baseline.md). Design (archived):
Magician as an App Platform.
Security boundary: [threat model](app-platform-threat-model.md).

## Owner-facing primitives

- **`app_discover`** is registry-only. Its prefix page is indexed and bounded (24 entries, 512 raw
  candidates, 8 MiB aggregate encoded rows before BLOB decode, a 250,000-step/100 ms SQLite
  progress fence); the normalized prefix is one sargable range and the installation index
  includes the enabled filter. Full entity/view/action schemas load only for one selected
  installation and are revalidated after package loading. Installations whose grant
  classification/model floor is ineligible for the physical model are concealed; actions whose
  runner, personality, tools or input schema are ineligible are not advertised. Discovery emits a
  sensitive, non-egressing metadata guard matching the re-attested caller, but no transferable
  source handle.
- **`app_data_query` / `app_data_search`** accept the canonical bounded predicate arena,
  ordering, opaque cursor and relation-expansion contract; the entity adapter and store stay the
  sole data/policy authorities. Purpose is server-stamped; `compare`/`in` exclude JSON null (use
  `is_null`); post-authority failures collapse to one guarded non-egressing unavailable outcome.
- **`app_action_invoke`** takes only a discovered installation/action, a stable idempotency key
  and typed input, and delegates to the canonical workflow service for contract, grant, runner,
  procedure, resource, binding and start admission.
  - A server-owned caller-family reference keeps retries stable across chat turns; an existing
    exact binding returns the same run handle without rematerializing mutable metadata, while a
    sealed ready task re-passes admission before starting.
  - Each new lane seals the full value-dependent input policy, inherited by run/result projection.
  - A move-only provider publication fence is re-attested at final read, binding, effect, start,
    result and serialization boundaries; denial keeps only a content-free
    unavailable/effect-uncertain stage and the newly attested profile gets neither run identity
    nor result (a fresh turn recovers with the same key).
  - Artifact task shells stay pending until the binding is durable; a crate-private app-only
    transition then readies the exact pristine shell before the final cancellation/fence/start. A
    ready shell with prior execution/output evidence is corrupt. The only binding-less recovery
    (shared with brokered composition) requires no disk/registry binding and no prior evidence.
  - Deterministic creation and root-start admission share one cross-process per-task lock
    (distinct from the reducer lock); the loser re-reads the committed task before events or
    spawn, and reducer commit refuses a stale active-root replacement. An opaque app-start permit
    carries the lock across the final provider/Stop fence into Artifact start. A waiter finding a
    started peer re-attests before returning it.
  - The model projection has no task/execution IDs, revisions, receipts, provenance or policy
    internals.
- **Authoring catalog and enablement.** `magician app tools|agents|personalities|procedure list`
  and `GET /api/magician/v2/apps/authoring/*` share one catalog. Owner enablement is
  `GET/POST /apps/installations/{id}/review|approve` and `magician app approve`: both hydrate the
  requested grant from the staged package, let the owner subset tools/agents/personalities, then
  build `AppReviewedInstallationCommit` before `publish_installation_approval` and
  `commit_reviewed_installation`. A same-package retry returns `already_enabled`. There is one
  lifecycle. See [app-authoring-cli](app-authoring-cli.md).

## Native widget projection and indicator materialization

`magician_v2::apps::widget_runtime` is the host-owned surfacing boundary. A batch request carries
only `(installation_id, widget_id)` targets and a closed client-capability set. The runtime
reopens the enabled installation, looks up an exact scope/package/generation-registered compiled
plan, and replays its cursor-free, relation-free entity query through
`AppEntityAdapterService::owner_query`; HTTP input cannot introduce a query, predicate,
parameter, binder or tool. Limits: 32 rows/32 fields per read, 64 KiB per native model, twelve
models and 1 MiB per page. Cache keys are scope, installation, generation and declaration id;
content revisions and ETags exclude observation timestamps.

Indicators use a bounded per-scope due queue with deterministic jitter and a pre-await recovery
schedule. Popping due work hides the old materialization before any await; only a successful
current-generation evaluation reinserts it. The GET route reads only these expiring
materializations. Lifecycle hiding clears render caches and indicators synchronously.

Only `rendering: native` closed view projections compile. Mini-frame fallback views are not
compiled (no separate fallback read is declared), and manifest indicators stay inert (no record
selector, predicate, aggregate or total-order proof; an unfiltered `LIMIT 1` would be
nondeterministic). Registration revalidates the package digest and installation tuple. Slot
inventory and system-default provenance: [app-default-surfaces](app-default-surfaces.md).

## Surfaces and computed capabilities

**Custom surfaces.** Isolation and bridge admission refuse general Iframe tokens; replay, stale
revision, forged origin and oversized messages fail closed. Surface bytes come only from exact
live `surfaces/` members of a verified package revision; the display envelope is no-script
`srcdoc` plus deny-all CSP. Authenticated host/asset/bridge routes serve that envelope, inline
admitted CSS, run admitted query/mutate/invoke on the owner data plane, enforce
message/byte/TTL/session watchdogs, and are torn down by the projection worker on disable,
quarantine, update or revocation. Scripted `surfaces/*.js` runs in a Magician-spawned OS child:
admitted `surfaces/` only, stripped env, no network, queued bridge calls, killed on lifecycle or
CPU/RSS/wall limits (budget polling walks only its process tree). The macOS profile allows default
reads (system libraries) but denies writes outside the sealed workdir. Wasm is refused. Unified UI
renders via `AppCustomSurfaceHost.svelte` with an empty sandbox (desktop embeds the same host;
`Iframe.svelte` is unchanged). Qualification: `make qualify-app-custom-surface-host`. Owners:
`sandbox.rs`, `surface_assets.rs`, `surface_host.rs`, `surface_runtime.rs`, `surface_worker.rs` in
`magician-apps/src/apps/`.

**Computed capabilities** come only from the exact package lock intersected with the current
grant (`magician/src/magician_v2/apps/capability_catalog.rs`). Disable, quarantine, revoke, retain
and purge hide the catalog; invented names fail closed. App-workflow USR pack dispatch must
present `authorize_computed_capability_dispatch`. Enabled installations project exact revisions
into the scoped USR catalog only when the locked document is USR-executable. A pack colliding with
a platform pack is **skipped, not substituted** — while app-origin dispatch outside workflows is
refused (below), an app shadowing `http` would brick `http` scope-wide. Disable evicts the overlay
and invalidates the scoped cache. Chat and personal-agent dispatch must present
`authorize_discovered_computed_capability` against the live overlay (≤ 100 installations and
100 tool names; ranking only reorders). No second executor.

**Direct owner turns.** A chat turn has no persisted task binding, and there is no credential for
"an autonomous agent acting on its own" (`TaskExecution` needs a binding; `SystemWorker` is
refused), so only a **direct owner chat/voice session** (`personal_agent_composition_is_direct`)
may invoke an app tool, contained by that app's grant. The governed `app_data_*` family is
narrower: only direct local chat owns its physical-profile and result-guard contract; realtime
voice excludes it. `AppWorkflowService::resolve_direct_owner_tool_authority` resolves the grant
through the same `resolve_app_authority` with a ceiling naming only the invoked tool, failing
closed on disabled/quarantined installations, revoked grants or ungranted tools. Delegated,
outward, meeting and autonomous turns are refused first. Gaps on this path: the disclosure permit's
network-destination check and result labels; resource ceilings need an ambient per-installation
ledger.

**App-origin dispatch is fail-closed off the app-workflow path.** Computed capabilities are
visible in the shared per-scope registry, but their authority (grant, network policy, resource
ceiling, byte-exact disclosure permit) is applied only inside the fenced app-workflow branch of
`execute_action_inner`. The scope snapshot carries `app_origin_tool_installations`, and the Pack
arm refuses an app-origin tool without `app_workflow_context`: authority belongs to the resource,
not the code path. Containing that path needs a per-turn permit carrying `ResolvedAppAuthority`
(see `docs/archive/plans/2026-08-20-app-tool-authority-remediation.md`).

**Local-only app memory in chat.** Ordinary callers admit only `remote_allowed` accepted
candidates. An authenticated direct chat turn may also render `local_only` candidates under a
runtime-only, non-serializable credential binding owner scope, session, turn, personal agent,
configured local profile, endpoint trust revision, config digest, expiry, local eligibility and
`no_provider_storage`. It is revalidated after retrieval's last async check and installed as
MagicLLM's disclosure guard, which revalidates the physical profile/endpoint cohort and authority
just before provider I/O. Profile switch, fallback, trust/config rotation, expiry, replay or
crossed session removes the bytes; these candidates never enter the utility-review queue. Realtime
voice, voice notes, delegated voice-to-chat and the desktop voice bridge stay fail-closed (their
protocols do not carry the owner credential plus profile fence).

App processing profiles may only name providers with a reviewed physical `no_provider_storage`
contract. DeepSeek, xAI and custom adapters are refused at config validation
(`enforce_app_processing_trust_invariant`) and at the boundary (`ProviderRetentionUnsupported`);
xAI keeps an unreviewed per-server prompt cache despite `store: false`.

**Tools.** Authoring, capability check, publication and VibeDev handoff refuse a
`skill_type: tool` document without a USR contract. Apps declare `dependencies.tools` by real
name; the lock snapshots an eligible reviewed skill (typed actions required) or typed compiled
pack with internal identity `capability:{name}`. Grant of the named tool is the control;
attestation is minted internally. Procedures, personalities, shell bins and raw-argv passthrough
fail closed as tools. The default runner `personal-assistant` is requested on the grant when a
workflow omits `agent`. Catalog and review mark lockable tools without a trusted dispatcher.
Dispatch is bind + contain + receipt, classified from schema, never re-classified from a friendly
name at re-review: [app-tool-bind](app-tool-bind.md).

## Lifecycle and authority

**Authority axes are compared exhaustively.** `compute_permission_diff`
(`magician-apps/src/apps/update.rs`) destructures both grants with no `..`, so a new
`AppGrantRevision` axis fails to compile until someone decides whether expanding it re-enters
review. Network destinations, background execution (going unattended or shortening the interval)
and context reads feed `requires_review`; `every_authority_axis_expansion_requires_review`
covers comparisons the compiler cannot. Update/reinstall reviews diff the active vs requested
grant. `authorize_update_switch` is test-only; commits go through the approve kernel.

The diff resolves the prior grant with `AppEntityStoreService::data_owner_schema`, **never
`active_schema`**: review happens precisely when the installation is not enabled (`UpdatePending`,
`UninstalledRetained`), and reading a grant to display it is not executing under it (purged stays
unavailable). Read failures propagate as `AppInstallationReviewError::EntityStore`.

**Owner kill switch.** Owner routes: `POST /apps/installations/{id}/disable`, `/quarantine`,
`/uninstall` (`UninstallRetain`), `/grant-revocations` (revoke + quarantine), `/update-begin`
(parks in `UpdatePending`) and `/update-abort` (back to Enabled/Disabled). They take a closed
`{expected_generation, request_id}` body (grant revocation also keeps the UI's legacy empty body,
which gets a server one-shot identity and is not response-loss safe). The client persists exact
bytes before POST; scope, actor, installation, generation, operation and request ID derive one
lifecycle-outbox identity, so response loss replays byte-identically. On full Apps-page reload
the client reposts retained intents (bounded, current scope) before accepting the directory, and
clears an intent only after an exact generation/status/review receipt.
`owner_kill_switch_routes_are_wired_to_their_handlers` keeps them reachable.

**Re-enable** has separate `GET /apps/installations/{id}/reenable-review` and
`POST /apps/installations/{id}/reenable`. The owner sees current package bytes/lock, grant,
schema, compiled surfaces, global policy and physical implementation digests and echoes the
sealed review digest; the service repeats source/lock review and the registry repeats every
identity in the same IMMEDIATE transaction as the `ReenableReviewed` CAS. Only an exact Disabled
generation with an unrevoked grant qualifies. Re-enable does not remove memory/retrieval
invalidation tombstones or republish old contributions. The live CLI calls these HTTP owners
rather than writing transitions (a CLI-process registry cannot evict the server's overlay); the
same applies to package export/import and purge preview/commit/status. Disable/revoke hide the
overlay before the lifecycle outbox is claimed.

## Module inventory

Kernel types and execution owners: `magician/src/magician_v2/apps/`. Route-facing, lifecycle,
surface, purge and projection owners: `magician-apps/src/apps/`. HTTP transport:
`magician-api/src/apps_api.rs`.

- Kernel: `models.rs`, `records.rs`, `registry.rs`, `schema_compiler.rs`, `entity_store.rs`,
  `entity_mutation.rs`, `entity_adapter.rs`, `package_staging.rs`, `package_transfer.rs`,
  `authoring.rs`, `lifecycle.rs`, `authority.rs`, `boundary.rs`, `approval_boundary.rs`,
  `policy.rs`, `manifest.rs`, `package_lock.rs`, `skill_dependencies.rs`, `query_semantics.rs`,
  `value_mapping.rs`, `artifact_selection.rs`, `memory.rs`, `portability.rs`,
  `resource_contract.rs`, `resource_authority.rs`, `workflows.rs`, `llm_operations.rs`,
  `capability_catalog.rs`, `app_tool_bind.rs`.
- `registry.rs` owns scoped SQLite identity, canonical package revision, the bounded blocking
  lane and atomic initial publication; additive migration owns generic record/index/receipt/
  outbox tables. Existing scopes migrate lazily; absent scopes stay unmaterialized. Standalone
  procedure revisions are write-once bytes with no mutable-skill or global-catalog fallback.
- `authoring.rs` owns `magician app init/check/test/pack` and mints no authority.
- Lifecycle/surfaces (`magician-apps/src/apps/`): `fixtures.rs`, `benchmark_fixtures.rs`,
  `entity_outbox.rs`, `entity_portability.rs`, `entity_retention.rs`, `registry_lifecycle.rs`,
  `threat_model.rs`, `retention.rs`, `update.rs`, `sandbox.rs`, `surface_assets.rs`,
  `surface_host.rs`, `surface_runtime.rs`, `surface_worker.rs`.
- Transport consumes only middleware-issued `VerifiedRequestIdentity`, binds the transport/session
  fence, exposes scoped installation/attempt reads, package import/export, bounded owner
  query/mutation over `AppEntityAdapterService`, and the reviewed direct-user action
  invoke/result projection. Server-owned schedule/event launch adapters are not mintable over
  HTTP. Imported packages are staged only. Data bodies cannot carry scope, a store fence or
  mutation provenance.

## Guarantees

### Storage, publication and staging

- `MAGICIAN_ROOT_DIR` (default `$HOME/MagicianNotes`) is pinned to its physical directory at boot.
  Registry roots, every descendant component and every SQLite-owned file reject symlinks at point
  of use. Registry construction touches no filesystem; the first authenticated write opens
  `<scope>/apps/app_store.sqlite3` on the blocking pool and binds principal/workspace in a
  singleton metadata row. Registry and stager share create-only `apps/scope-binding.json` so a
  collision fails before rows or bytes exist.
- Initial publication accepts only a non-deserializable value minted from an admitted bundle, its
  immutable dependency lock, a validated package revision, a conformance-complete attempt and a
  generation-one inert installation, committed in one `IMMEDIATE` transaction. Exact replay is
  idempotent; different bytes under a reused identity roll back. Reviewable rows grant nothing.
- Portable archive v2 carries the full private-field lock beside its digest; the deserializer
  revalidates versions, identity, ordering and digest but treats it as an untrusted claim.
  Candidate publication reloads every registry procedure revision under the authenticated scope
  and rebuilds the lock. Package-revision identity includes the lock digest; immutable
  `(package_id, semantic_version)` blocks silent dependency replacement.
- Reviewable publication consumes the staged-package proof and revalidates the content-addressed
  directory just before the transaction. Approval publication rejoins live scope/actor/session,
  auth revision and current global-policy revision to the attempt, package and requested
  authority. Reviewed activation repeats this in an `IMMEDIATE` transaction, validates the
  grant/schema/surface successor set and policy/resource/schema/migration digests, consumes the
  approval once, commits attempt and generation, and appends the metadata-only lifecycle event.
- Enable, disable, update-begin/fail, quarantine, retained uninstall and grant revocation are
  generation CAS; revocation and quarantine share one transaction. Outbox claims/acks use private
  move-only lease tokens with finite limits and stale-owner rejection; recovery is explicit,
  per-scope and bounded, never a startup scan. Entity-change projection has its own claim-driven
  lease owner (expired leases reclaimed by a later claim; stored digest rejoined before delivery).
- Registry access needs a live server-minted `AuthenticatedAppScope` (never deserializable; minted
  from a verified session or the explicit single-user loopback fallback checked against the real
  peer IP — forwarded headers are not evidence). SQLite never runs on an async worker; blocking
  work has a finite admission ceiling; writes serialize per scope with a finite wait; background
  writers have a separate ceiling that cannot take the last foreground lane.
- Package staging needs the authenticated scope and a blocking permit, opens every component
  no-follow and descriptor-relative with retained directory authority, reopens `.` per inventory
  scan, and rejects owner/filesystem/permission/link/type/identity/timestamp/inventory drift.
  Depth, entry, file, path, member and byte limits are finite. Bytes land in a private unique
  sibling, everything is flushed, and a create-only rename publishes the bundle-digest directory.
  A directory-sync failure after rename is explicit commit-state-unknown; authenticated recovery
  removes only stale `.staging-*` trees. Nothing runs at startup.

### Manifest, bundle and dependencies

- The procedure loader reads bounded routing metadata first; declared apps are validated by the
  strict parser and excluded from procedure discovery (malformed apps cannot fall back). App
  workflows and private procedures add no global procedure/tool descriptors.
- App YAML is a small V1 subset: bounded bytes, indentation and lexical depth (compact `- - -`
  nesting and mapping markers count); no aliases, anchors, tags, merge keys, block scalars or
  extra documents; an iterative decoded walk enforces node/scalar/depth ceilings; unknown fields
  rejected throughout.
- Bundle admission takes a complete member inventory and rejects unsafe/absolute/traversing
  paths, compatibility-character escapes, non-portable non-ASCII paths, case collisions, links,
  special files, oversized members and missing prompt/asset/vendor references. Production
  ceilings are fixed. Members are move-only; candidate clones share verified buffers via `Arc`.
  Staging caps readers before allocation; bundle identity covers every path, length and digest.
- Dependency locks accept no name-only or self-asserted digests: registry evidence is
  non-deserializable and minted from an exact positive revision and computed digest. Vendored
  procedure identity and `vendor/skills/<name>/SKILL.md` derive from the manifest, and the
  digest covers the whole subtree. Registry-backed skill dispatch must present the exact locked
  revision and bytes; vendored dispatch resolves only from the admitted subtree. Golden digests
  catch unversioned canonicalization drift.
- A workflow prompt is read only from its package member. Procedures come from registry-minted
  exact bytes or the rehashed vendored subtree; invocation rejoins package, lock and authority and
  gives each procedure only `allowed-tools` ∩ workflow authority (omitted = inherit the narrowed
  set). Prompt/procedure bodies are never serialized or globally cataloged.
- Workflow capabilities need an exact declared dependency and version; no implicit `*`.
  Data-plane production limits have private fields and no widening builder.
- V1 views are semantically typed (Table columns exist and are unique; Tree bindings have exact
  field shapes; Timeline needs timestamp/action bindings; Graph needs a nullable self-reference
  parent and a label field; List rejects foreign bindings). Routes use a portable ASCII grammar,
  reject encoded/Unicode traversal, and reserve `/apps` case-insensitively. Hostile JSON is
  rejected by byte/depth/node preflight; predicates use a flat arena with iterative validation.

### Authority and execution

- Authority resolution checks scope, installation generation and active
  package/grant/schema/surface revisions; disabled, quarantined, retained, purged, expired,
  revoked, malformed or stale inputs fail closed. Tools and context reads are intersections of the
  app grant, acting-agent, trust and optional parent ceilings (empty = deny). Classification takes
  the most restrictive floor; model processing, personal-agent access, memory promotion, egress,
  background execution, destinations and every resource dimension only narrow; a zero resource
  ceiling means denied. The authority digest binds actor, session, auth revision, generation,
  every revision and axis, and the grant digest. Digests are `blake3:<64 lowercase hex>`.
- A workflow action is a normal V3 task with an id derived from its invocation idempotency
  identity; its registry control head (not a sidecar) authorizes task/run/result state. A
  committed result short-circuits before any new execution or admission. Superseded control
  payloads are pruned transactionally. Ordinary V3 tasks never open app workflow-control state.
  Schedule/event sources need a short-lived non-deserializable launch proof.
  Delegated/retry/resume/repair records reuse the root binding and budget. A terminal commit
  persists its intent before reservation, revalidates authority and permit lifetime just before
  entity-store I/O, and publishes output only after settlement; mutation idempotency is
  task/output-revision based.
- Model admission binds labels and package/grant/schema authority to one endpoint, model,
  transport cohort, retention posture and continuation partition. Protected requests are cold,
  non-streaming and one-attempt (no fallback, retries, reuse, stateful continuation or caches);
  reviewed adapters force storage off and never follow redirects; unsupported/custom providers
  and aggregators without an attested route fail closed. The disclosure window is ≤ 10 minutes
  (crossing it revalidates source/profile/partition, endpoint config and registry authority). The
  LLM permit holds the root owner through the last async check and samples expiry before I/O;
  usage uses checked arithmetic and missing observations settle uncertain. Tool egress uses a
  separate move-only permit over authority, labels, destination, live endpoint and exact outbound
  bytes. Results re-enter model continuation only via a server-labeled checkpoint and
  protected-retention permit. Generic memory, task state, local preparation, reviewer,
  reflection, capture/eval, work-ledger and diagnostics receive only lifecycle metadata.
- Data envelopes verify `content_digest` against the canonical typed value at query-page and
  action input/output validation (streamed BLAKE3). Wire labels and policies are claims; joins and
  consequential consumers accept only trusted labels plus an effective policy resolved at the
  exact final boundary, paired with a borrowed revalidated envelope.
- Typed query evidence binds every selected, filtered, ordered and relation field; ordering has
  an implicit record-ID tiebreaker. Server-stored cursor evidence binds installation, query/order
  digests, schema revision, retained dataset generation, last record and validity window; clients
  cannot mint it. Selection and continuation each use one SQLite snapshot; continuation keeps the
  original window and deterministically publishes its child (response-loss idempotent). The
  retained generation describes snapshot membership, not the dataset head, so unrelated edits do
  not invalidate it, but every continuation consumes current authority and requires each returned
  record's exact live revision. Details: [app-entity-store](app-entity-store.md).
- Mutation batches are all-or-nothing with an idempotency key; optimistic revisions cover every
  update/delete/restore target; duplicate record/edge writes are rejected; relation writes carry
  both endpoint revisions. Reviewed install/update/reinstall need a move-only approval fence
  bound to approval revision, attempt kind, package, authority/policy/schema/migration digests,
  global policy revision and live scope/actor/session/kind/revision; durable compare-and-consume
  owns the approval. Store fences bind the digest of the exact validated query or mutation.

### Values and composition

- Workflow values use one content-addressed bounded graph shared by manifest admission, Recipe
  IR, launch/result validation, composition and SDK codecs
  ([app-recipe-ir](app-recipe-ir.md)). Result declarations correlate exact typed-value,
  entity-projection, artifact-reference or receipt-reference roots; bridges never substitute
  task/execution or raw artifact IDs.
- Deterministic composition is a flat bounded select/rename, constant, total enum map and
  registered-conversion program — never code. Compilation rejects optional→required,
  nullable→non-nullable and narrowing enum selects against reviewed schemas (not the current
  value); execution validates the full source, omits absent optional mappings and propagates
  allowed nulls; mapping digest and schema identity are exact; unknown output fields are rejected.
  The only model-derived seam couples that validator to the canonical policy/provenance join.
- Action-result composition is policy guarded; transfer receipts and digests stay sealed in task
  bindings. Logical transfer identity excludes the transient chat execution. A source without a
  result is `waiting` only while its task is nonterminal; failed, cancelled, archived and uncertain
  sources settle as guarded terminal outcomes.

### Contract crate and codegen

The pure `magician-app-contract` crate owns public versions, compatibility, operation metadata and
capability DTOs (`models.rs` keeps runtime wire types not yet extracted). `app-contract-codegen`
emits Draft 2020-12 schemas, the operation inventory, supported-public OpenAPI 3.1 paths, a
versioned contract manifest, JSON goldens and TypeScript/Swift fixture catalogs under
`docs/contracts/app-platform/v1`, Unified UI and `magios/Shared`; route registration consumes the
same path/method metadata. `make app-contract-codegen` regenerates; `make app-contract-check`
detects drift and runs Vitest and Swift round trips.

### Derived content, memory, portability, retention

- Derived content joins trusted classification upward and model permission downward, keeps exact
  source identities and yields deterministic policy/provenance/content digests. Locality comes
  from a current server-owned endpoint attestation, never provider/model names; tool egress is an
  independent exact-destination check. Hidden consumers are enumerated (unlabeled bytes denied;
  label-blind, analytics and crash diagnostics get metadata; model-backed consumers pass the
  endpoint decision). Provider continuations are partitioned by scope, installation, revisions,
  endpoint/trust/config, model, policy, authority and handling.
- App records do not become memory by being written. Candidates come only from server-joined
  content plus exact current record evidence and keep full identities and labels;
  `model_processing: none` or revoked promotion fail closed. Proposed candidates are not
  retrievable; retrieval needs a fresh non-deserializable `AppMemoryEligibilityFence` for every
  source. Drift → stale; disable/quarantine/update/uninstall-retain → dormant; sole-source
  delete/purge → tombstone; multi-source removal needs a newly reviewed candidate; stale and
  tombstoned never return to accepted. Source evidence binds the same actor, session and auth
  revision as the resolved authority. Contributions:
  [app-memory-contribution-storage](app-memory-contribution-storage.md).
- Package archives contain only immutable members, lock/compatibility identity and advisory
  verification evidence; data archives only typed records, schema/package digests, ordinal
  aliases, provenance and selected attachments — neither has scope, installation, grant,
  credential, schedule, memory or provider-session fields. Data/combined archives default to a
  versioned XChaCha20-Poly1305/Argon2id streaming envelope; plaintext is a separate session-bound
  warned approval; secret-class data is never eligible. The archive writer rechecks live scope,
  actor, session and auth kind/revision; approvals cannot outlive their session.
- A package digest proves bytes only. Signature-chain evidence is non-deserializable,
  byte-bound and accepted only with the current trust-registry revision; unsigned imports get a
  new local-fork identity. Every import repeats digest verification, conformance, permission
  review and required rebuild/sandbox qualification; foreign grants never transfer. Data import
  previews cover every record, allocate local IDs, expose merges/conflicts/rejected fields and
  missing attachments, and bind destination scope, installation ID and generation through
  approval, replay and receipt. Limits apply before sorting or hashing.
- Retention has positive time and byte ceilings for revisions, WAL/temp, backups/exports,
  attachments, captures, eval artifacts and audit tombstones, plus a per-record revision ceiling;
  personal, sensitive and secret data need a scope-bound key. Purge is a 17-class multi-store
  protocol (canonical rows, WAL/temp, indexes, package/cache, attachments/exports/Artifact V2,
  memory, analytics/captures/evals, provider continuations, routes/surfaces, schedules/outbox,
  disclosure, directory/search), each settling deleted, cryptographically erased,
  shared/policy retained, provider unknown or failed. Partial cleanup never becomes `purged`; only
  the retention owner applies that transition after matching a complete receipt. Storage
  Governance inventories app storage read-only with capped scans and reports partial inventory
  rather than undercounting; the inventory itself deletes and encrypts nothing.
- Forget protocol V2 persists selection kind plus digest, never raw record IDs. Export and import
  preview share a 10,000-record ceiling. The fixed qualification profile
  (`benchmark_fixtures.rs`) streams 10,000 installation records, 100,000 scope records and 1,000
  outbox rows without duplicating the dataset.

### Resource authority

- Resource journals are stored claims, never authority. Replay needs a fresh non-deserializable
  fence binding scope/session, installation generation, package/grant/schema/authority revisions,
  root execution, ledger and period, minted at the decision time with a live session and a
  same-boundary authority resolution; the period snapshot excludes the current root. Historical
  replay validates what the journal accepted against tree-local ceilings, not today's concurrency.
  New root/reservation verdicts authorize nothing until the authority appends them atomically
  under the same period revision and yields a move-only permit; exact replay never yields a second
  permit. One shared scheduler gate keeps foreground capacity, prevents two live leases per root,
  and keeps resumed roots on their original period, deadline and elapsed-time origin.
- Root, child, retry, resume, repair, tool/browser, synthesis and reflection work form one flat
  bounded tree with deterministic, iteratively checked identities and topology (no recursive
  walk; a counted expiry index avoids rescans). Units add with checked arithmetic (cached input is
  a subset of input); parallel active time is an incremental interval union; each outstanding
  reservation contributes its full deadline window before dispatch; committed intervals must sit
  inside their reservation; crash recovery derives windows from the journal, not telemetry.
  Opening retries does not manufacture progress.
- Retention closes periods before compacting terminal trees, inserts an immutable replay-denial
  tombstone before deleting the journal, and runs only on the background writer lane (≤ 32 trees
  and 16 MiB per transaction; the keyset driver requests eight). Work reserves against committed
  plus outstanding tree and period spend. Over-reservation usage is rejected unexposed and held.
  Expiry never releases an uncertain effect; `proven_unspent` needs the exclusive crash-reconciler
  source plus separate recovery evidence.
- **Crash settlement for bound effects.** A crash-recovery owner may conservatively commit the
  exact reserved upper bound for an already dispatched effect, keeping its effect identity, with
  only the `crash_reconciler` source and no provider-result digest or bytes. It cannot claim
  unspent work, add a physical observer, fabricate output or authorize dispatch; the crash proof,
  accepted authority, journal revision, reservation bounds and leaf-first closure still apply.
  Journal validation and replay accept this shape so an uncertain effect cannot hold a resource
  tree (and a concurrency slot) open forever; the original uncertain event and failed status are
  preserved. Tests: `make test-app-resource-cleanup`.
- Token, MagicLLM task, VibeDev active-time, tool, browser, app-store, attachment and package
  owners are named observation sources only; none can mint a reservation or answer remaining
  budget.

## Required memory adopters

`AppMemoryEligibilityFence` is the canonical source-eligibility carrier, not an app-only side
table. Memory owners carry or reference its candidate identity and fence:

- `magician-vector-index/src/memory_candidates.rs` — persist exact app source revisions and
  handling identity on the canonical candidate
- `magician-vector-index/src/memory_index.rs` and `memory_hot_projections.rs` — exclude
  candidates without a current fence and invalidate indexed/hot copies on source revision or
  lifecycle changes
- `magician-vector-index/src/memory_temperature.rs` — keep temperature a utility overlay only;
  it cannot restore source eligibility
- `magician/src/magician_v2/agents/memory_consolidator.rs` — nominate and promote through the
  source-linked candidate contract
- `magician/src/magician_v2/agents/memory_prompt_blocks.rs` — synchronously recheck the fence
  before prompt inclusion

The app-store resolver in `magician/src/magician_v2/apps/memory_store.rs` mints current source
evidence from the scoped registry. Prompt, search and code-knowledge retrieval compare the
envelope to that live snapshot; neither an outbox nor a cached serialized candidate is
current-source authority.

## Claims-decision command seam (evidence queue 1, additive)

`magician.claims-decision` is an authoritative command family, separate from the hypothesis-only
contribution terminal. Closed verbs: `confirm_claim`, `reject_claim`, `record_commitment`,
`confirm_commitment`. Typed targets seal the observed revision (commitments also their full
audience address). Proposal, owner review and decision envelope bind source provenance, exact
display, authenticated scope, named actor, optional note, disposition and a stable canonical
decision id under domain-separated digests and the trusted desktop signature; the signed review
also seals the active desktop pairing generation (re-pairing staleness older envelopes).

The proposal uses a dedicated claims-command source/authority header (not the generic
contribution header), sealing installation generation, package, grant, schema, workflow, action
and the exact `review_decision` source head; contribution-port, evidence-class, settlement and
label fields are absent so hypothesis vocabulary cannot be mistaken for claims authority.

`claims_decision_contribution.rs` accepts only a live server-minted `AuthenticatedAppScope` whose
binding and `actor_ref` match the signed command, verifies lifetime/signature/contract, reopens the
pairing generation, destination schema, package workflow/action declarations, grant/schema tuple
and current source revision/payload digest, then calls only the receipt-bearing expected-revision
claim and commitment methods (scope and lifetime resampled under the destination locks). The
transition and receipt share one authoritative JSONL record; exact retry returns
`already_applied`; a changed request under one decision id or a stale head is refused. Package
`review_decision` and `review_receipt` entities are projections, never authority.

Commitment decisions first bind their identity in a deterministic scope-wide index (so one id
cannot be replayed against another relationship); completed entries carry receipts with audience
and confirming actor for non-mutating retry recovery. The hash-ordered index is not a projection
cursor; lossless projection waits for a monotonic completion journal.

## Meeting-control command seam (meetings queue 5, additive)

`magician.meeting-control` follows the claims shape with its own contract id, `control_request`
source entity and digest labels (neither family validates the other's envelopes). Verbs `listen`,
`join`, `pause`, `resume`, `stop` over two disjoint targets: `new_capture` (optional
url/title/date plus explicit `capture_mic`) for the start verbs, and `live_session` (session id
with rail prefix) for control verbs — a naked string could be either, and "either" is how a stop
becomes a start.

- **Intent is separate from authority.** Every start carries an `AppMeetingControlGestureV1`
  (bridge session, unique act id, expiry ≤ `APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS`, two minutes)
  sealed into the proposal digest and the owner's signed display. Risk-reducing verbs must not
  carry one (a stop would otherwise read like an intent-bound start).
- **The gesture is part of the decision id.** A capture start is not idempotent (reapplying later
  opens a new session), so two acts are two decisions, and the destination rechecks gesture
  freshness against its own clock. A signed start cannot be banked.
- **No new creation path.** `meeting_control_contribution.rs` reaches capture only via
  `join_meeting_with_scope` and `start_passive_listener` (the first-party `/meetings` entries)
  plus the managers' own pause/resume/stop. A source-level pin asserts those names are present
  and that `meeting_manager().join(`, `meeting_manager().spawn(`,
  `passive_meeting_manager().spawn(`, `PassiveMeetingSession::new(` and `MeetingSession::new(`
  are absent. A start is refused while any capture is live on either rail; every accepted and
  refused command lands in the shared capture-control audit.

Package `control_request` and `control_receipt` entities are projections: the request keeps
`actor_ref` null at `apply_state: recorded`, the host stamps the actor at admission, and only a
signed destination result produces a receipt.

## Attention-lane contribution port

The third destination-owned port in `magician-app-contract/src/contribution.rs` is
`magician.attention.candidate`. Apps never own attention state; they propose lane cards and the
owner decides through the memory port's sealed-proposal shape. A proposal carries the shared
contribution header (one source head, replace-exact update, tombstone-on-drift), a proposed lane
from a closed vocabulary matching core `AttentionLane` wire values, a closed urgency claim (card
content, not ranking authority), bounded title/summary, and the header `dedupe_key` as
idempotency key. Owner review and decision reuse the desktop-signed, head-bound envelope family
under `magician.attention-*` digests; receipts seal via `impl_sealed_receipt`. The additive
`attention_lanes_v1` manifest feature gates it (added in supported-public capabilities contract
`1.4.0`; the current version is `APP_SUPPORTED_PUBLIC_CONTRACT_VERSION`).

`magician/src/magician_v2/attention_lane_contribution.rs` holds fail-closed admission validation
(mirroring the memory port's `StageProposal` guard), a domain-separated idempotency key, and the
adapter rendering an accepted candidate as one `AttentionLaneItem` for the shared lane facade
(the destination owns ordering fields: acceptance time, neutral `other` source kind/family,
bounded provenance). Durable staging, owner-review listing and the dispatch outbox are not built
here; they belong to the Channel Assist consumer.

## App LLM operation lane

App packages declare named LLM operations so prompts route like core lanes — through the
operator-owned `llm.router.operation_mapping` with the same observability, budgets and locality
policy. Fail-closed at every seam:

- **Manifest.** `app.llm_operations` (gated by `llm_operations_v1`;
  `magician-app-contract/src/llm_operations.rs` owns `AppManifestLlmOperation`: a bounded
  owner-reviewable purpose plus an optional narrowing token hint). `manifest.rs` rejects the block
  without the feature, malformed purposes or zero hints; review re-parses through the same kernel.
  A manifest declaring nothing keeps its canonical digest.
- **Operator admission.** `app_platform.llm_operations` (`magician/src/config.rs`) lists admitted
  names, each with a reviewed purpose and a positive `max_output_tokens` ceiling (no default;
  config load rejects zero, and an absent ceiling still parses but the operation fails closed at
  resolve with `MissingOutputTokenCeiling`); a name absent from the policy is rejected for every
  app. The 256-entry cap
  (`APP_LLM_OPERATION_MAX_ADMITTED`) binds at config load (package declarations are bounded only
  by `max_collection_items`). Config load requires one `app:<name>` entry in
  `llm.router.operation_mapping` per admitted name, with every selector arm (`default`,
  `when_has_images`, `when_cloud`) naming a profile declared in `app_platform.processing.profiles`
  and present in `llm.router.profiles`.
- **Server half.** `apps/llm_operations.rs` owns the `app:`-namespaced key (core keys are bare
  `snake_case`; manifest names use the `AppName` alphabet, so no collision or second segment),
  `admit_manifest_llm_operations` (revalidates every name against live policy and mapping), and
  `resolve_admitted_app_llm_operation_profile` (rechecks the live admission list first, so a
  since-removed name fails closed, then resolves locality- and shape-aware through the trusted
  profile family and returns the live trust declaration for attestation).
- The effective output ceiling is `min(operator ceiling, manifest hint, physical profile,
  remaining run authority)`: the first three intersect in
  `resolve_admitted_app_llm_operation_profile`, and `apps/llm_dispatch.rs` clamps to the
  request's remaining output-token budget (zero → `CausationBudgetExhausted`).

Nothing grants blanket LLM access: admitted operations still go through the processing
boundary's disclosure, budget and locality machinery and the per-app data policy. Behavior
dispatch rules: [app-behavior-recipes](app-behavior-recipes.md) and
[background behaviors](app-background-behaviors-threat-model.md).

## Activated composition and run-control boundary

Query and search return short-lived projection handles bound to actor, scope, session, execution,
source revisions, policy influence, expiry and content digest — lookup keys, not bearer
authority, byte-bounded and quota-partitioned by scope and session under a global ceiling. Record
and action-result composition reopen both installations, compile the mapping against
destination-owned schemas, persist a destination-correct brokered invocation, and call the
workflow owner. An empty present-field fence is valid for constant/control mappings and never
relaxes destination validation.

The move-only owner credential is minted once per authenticated HTTP request with a
server-generated request reference: its execution reference is stable across that request's
governed calls, and a different request gets a different reference even if a client reuses a
chat-turn id, so client correlation IDs cannot alias another request's handles. Transfer identity
is stable across physical chat executions; same-key retries resolve to one destination task.
Permanent conflicts, transient failures and effect-uncertain failures are distinct; immutable
task/result/manifest/schema/fence mismatches are corruption; only a legitimate authority change is
stale authority.

Workflow results and receipt sidecars keep full provenance; the model projection is a separate
minimal DTO without task/execution/transfer IDs, fences, field sets, source refs, labels or
receipt content. A result guard is integrity-checked and admitted against the actual successful
physical profile and direct-owner audience just before bytes enter a model transcript. Chat keeps
a host-only joined policy and lineage; any generic model-visible tool result makes V1 lineage
incomplete, while a typed no-effect app outcome allows an exact same-source retry. Effectful
multi-source or unrepresentable derivations are refused. Realtime/external snapshots, search
catalogs, dispatch, delegated/handover rosters, introspection, autonomous policy snapshots and
tool-search working sets all exclude governed app tools via one exposure predicate, and manifest
and launch admission reject workflows naming one. Registration in the compiled-provider registry
is never proof of a guard-preserving consumer.

A chat Stop may return a bounded safe settlement receipt only when the governed provider returned
a typed outcome with its non-serializable result-policy guard; pre-provider cancellation
(including `CompiledDispatchCancelled`) and earlier refusals are ordinary cancellation;
post-boundary dispatch is non-droppable until its typed outcome is known.

Canonical `task_app_<digest>` lifecycle is owned only by app run control and exact
runtime-outcome settlement. Generic task creation, manifest edits, planning, scheduled/manual
launch, archive/delete, raw status, execution status override, generic launch, execution
cancel/delete and generic pause/resume/steer fail closed. The innermost runtime boundaries
classify both the reserved namespace and the sealed binding (only a non-canonical task with no
binding is ordinary), and namespace guards run before lookup. Artifact V2 exposes narrow
crate-private app seams for deterministic creation, launch and delegated parent reactivation; app
terminality projects only from the exact runtime outcome. Delegated continuation re-enters the
workflow owner for fresh admission; continuation-launch errors settle an unowned runnable parent;
forced-child shutdown commits one cancellation outcome. The Apps UI uses `run_ref` as control
identity, stores only sanitized, scope-partitioned, byte-capped run metadata, can resume polling
after reload, and discards malformed status/terminality/withheld combinations.

Memory-candidate publication is decided inside the per-scope write transaction: an idempotent
replay is recovered before Stop is consulted; only a missing candidate can return
`cancelled`/`effect_committed: false`; new and recovered proposals report
`publication_outcome: published` / `recovered` with `effect_committed: true` under the
source-policy result guard.

## Reconciliation source retention

Native reconciliation derives a source projection from its compiled declaration before
dispatch, keeping the union of source keys and fields used by all targets bound to that source
(`source_document` keeps the whole row). Page membership, order, cursors, completeness evidence
and truncation flags are unchanged. The raw response must fit its transport ceiling first; the
effect owner then seals and settles the compact projection under the existing policy and
continuation limits. No caller parameter, API bypass, model call, package edit or re-review is
involved. Unused source fields therefore cannot bloat continuations, while oversized mapped
results still fail explicitly (reported as a size error, not a corrupt binding) and uncertain
dispatch keeps its reservation. Tests: `make test-app-source-projection`.

## Known gaps

Complete workflow-local reads, remaining public SDK bindings beyond the manifest-derived
authoring types, remaining resource-authority adopters and threat-matrix rows, and the
load-bearing purge/retention/encrypted-stream adapters in Storage Governance and the
archive/crypto owner. Declarative migrations refuse an unreviewed authority expansion before
pointer switch.
