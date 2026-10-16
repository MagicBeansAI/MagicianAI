# App Platform Contract Kernel

Living contract for the app-platform kernel: types, owners, routes and
invariants. Status vocabulary, Artifact-to-app run mapping, canonical error
dispositions, payload-minimal trace fields, storage-owner obligations and canary
assignments live in the [App Platform truth
baseline](app-platform-truth-baseline.md). Design (archived): [Magician as an
App Platform](../../archive/plans/2026-08-07-magician-app-platform-design.md).
Security boundary: [threat model](app-platform-threat-model.md).

## Owner-facing primitives

- **`app_discover`** is registry-only, with an indexed, bounded prefix page (24
  entries, 512 raw candidates, 8 MiB aggregate encoded rows before BLOB decode,
  a 250,000-step/100 ms SQLite progress fence). Full schemas load only for one
  selected installation, revalidated after package load. Installations whose
  grant classification or model floor is ineligible for the physical model are
  concealed; actions with an ineligible runner, personality, tools or input
  schema are not advertised. Discovery emits a sensitive, non-egressing metadata
  guard matching the re-attested caller, never a transferable source handle.
- **`app_data_query` / `app_data_search`** accept the bounded predicate arena,
  ordering, opaque cursor and relation expansion; the entity adapter and store
  are the only data and policy authorities. Purpose is server-stamped;
  `compare`/`in` exclude JSON null (use `is_null`); failures after authority
  collapse to one guarded, non-egressing unavailable outcome.
- **`app_action_invoke`** takes only a discovered installation/action, a stable
  idempotency key and typed input, and delegates contract, grant, runner,
  procedure, resource, binding and start admission to the workflow service.
  - A server-owned caller-family reference keeps retries stable across chat
    turns; an existing exact binding returns the same run handle; a sealed ready
    task re-passes admission. Each new lane seals its full input policy,
    inherited by run/result projection.
  - A move-only provider publication fence is re-attested at final read,
    binding, effect, start, result and serialization; denial leaves only a
    content-free unavailable/effect-uncertain stage, and the newly attested
    profile gets neither run identity nor result (a fresh turn recovers with the
    same key).
  - Artifact task shells stay pending until the binding is durable, then an
    app-only transition readies the pristine shell; a ready shell with prior
    execution/output evidence is corrupt. Binding-less recovery requires no
    disk/registry binding and no prior evidence.
  - Creation and root-start share one cross-process per-task lock (not the
    reducer lock); the loser re-reads the committed task before events or spawn;
    reducer commit refuses a stale active-root replacement. An opaque app-start
    permit carries the lock across the final provider/Stop fence; a waiter
    finding a started peer re-attests it.
  - The model projection has no task/execution IDs, revisions, receipts,
    provenance or policy internals.
- **Authoring catalog and enablement.**
  `magician app tools|agents|personalities|procedure list` and
  `GET /api/magician/v2/apps/authoring/*` share one catalog. Enablement
  (`GET/POST /apps/installations/{id}/review|approve`, `magician app approve`)
  hydrates the grant from the staged package, lets the owner subset
  tools/agents/personalities, builds `AppReviewedInstallationCommit`, then runs
  `publish_installation_approval` and `commit_reviewed_installation`. A
  same-package retry returns `already_enabled`. See
  [app-authoring-cli](app-authoring-cli.md).

## Native widget projection and indicator materialization

`magician_v2::apps::widget_runtime` is the host-owned surfacing boundary. A
batch request carries only `(installation_id, widget_id)` targets and a closed
client-capability set; the runtime reopens the enabled installation, looks up
the exact scope/package/generation-registered compiled plan and replays its
cursor-free, relation-free query through `AppEntityAdapterService::owner_query`.
HTTP input cannot add a query, predicate, parameter, binder or tool. Limits: 32
rows and 16 fields per read (`APP_WIDGET_QUERY_MAX_ROWS`,
`APP_WIDGET_MAX_READ_FIELDS`), 64 KiB per native model, twelve models and
1 MiB per page. Cache keys are scope, installation, generation and declaration id;
content revisions and ETags exclude observation timestamps.

Indicators use a bounded per-scope due queue with deterministic jitter; popping
hides the old materialization before any await, and only a successful
current-generation evaluation reinserts it. The GET route reads only these
expiring materializations. Lifecycle hiding clears render caches and indicators
synchronously.

Only `rendering: native` closed view projections compile. Mini-frame fallback
views are not compiled; manifest indicators stay inert (no record selector,
predicate, aggregate or total-order proof, so an unfiltered `LIMIT 1` is
nondeterministic). Registration revalidates the package digest and installation
tuple. Slots and system defaults:
[app-default-surfaces](app-default-surfaces.md).

## Surfaces and computed capabilities

**Custom surfaces.** Bytes come only from exact live `surfaces/` members of a
verified package revision, displayed as no-script `srcdoc` with deny-all CSP.
Isolation and bridge admission refuse general Iframe tokens; replay, stale
revision, forged origin and oversized messages fail closed. Authenticated host,
asset and bridge routes serve that envelope, inline admitted CSS, run admitted
query/mutate/invoke on the owner data plane, enforce message/byte/TTL/session
watchdogs, and are torn down by the projection worker on disable, quarantine,
update or revocation. Scripted `surfaces/*.js` runs in a Magician-spawned OS
child: admitted `surfaces/` only, stripped env, no network, queued bridge calls,
killed on lifecycle or CPU/RSS/wall limits (polling only its process tree); on
macOS, default reads are allowed and writes outside the sealed workdir denied.
Wasm is refused. Unified UI renders via `AppCustomSurfaceHost.svelte` (empty
sandbox; desktop embeds the same host; `Iframe.svelte` unchanged).
Qualification: `make qualify-app-custom-surface-host`. Owners in
`magician-apps/src/apps/`: `sandbox.rs`, `surface_assets.rs`, `surface_host.rs`,
`surface_runtime.rs`, `surface_worker.rs`.

**Computed capabilities** are only the exact package lock intersected with the
current grant (`magician/src/magician_v2/apps/capability_catalog.rs`); disable,
quarantine, revoke, retain and purge hide them; invented names fail closed.
App-workflow USR pack dispatch presents
`authorize_computed_capability_dispatch`. Enabled installations project exact
revisions into the scoped USR catalog only when the locked document is
USR-executable. A pack colliding with a platform pack is **skipped, not
substituted** (an app shadowing `http` would break `http` scope-wide). Disable
evicts the overlay and invalidates the scoped cache. Chat and personal-agent
dispatch present `authorize_discovered_computed_capability` against the live
overlay (≤ 100 installations and 100 tool names; ranking only reorders). No
second executor.

**Direct owner turns.** A chat turn has no task binding (`TaskExecution` needs
one; `SystemWorker` is refused), so only a **direct owner chat/voice session**
(`personal_agent_composition_is_direct`) may invoke an app tool, within its
grant. `app_data_*` is direct local chat only, not realtime voice.
`AppWorkflowService::resolve_direct_owner_tool_authority` calls
`resolve_app_authority` with a ceiling naming only the invoked tool, failing
closed on disabled/quarantined installations, revoked grants or ungranted tools.
Delegated, outward, meeting and autonomous turns are refused first. Gaps: the
disclosure permit's network-destination check and result labels; resource
ceilings need an ambient per-installation ledger.

**App-origin dispatch is fail-closed off the app-workflow path.** Computed
capabilities are visible scope-wide, but their authority (grant, network policy,
resource ceiling, byte-exact disclosure permit) applies only inside the fenced
app-workflow branch of `execute_action_inner`. The scope snapshot carries
`app_origin_tool_installations`, and the Pack arm refuses an app-origin tool
without `app_workflow_context`. Planned containment: a per-turn permit carrying
`ResolvedAppAuthority`
(`docs/archive/plans/2026-08-20-app-tool-authority-remediation.md`).

**Local-only app memory in chat.** Ordinary callers admit only accepted
`remote_allowed` candidates. An authenticated direct chat turn may also render
`local_only` candidates under a runtime-only, non-serializable credential
binding owner scope, session, turn, personal agent, configured local profile,
endpoint trust revision, config digest, expiry, local eligibility and
`no_provider_storage`. It is revalidated after retrieval's last async check and
installed as MagicLLM's disclosure guard, which rechecks the physical
profile/endpoint cohort and authority just before provider I/O. Profile switch,
fallback, trust/config rotation, expiry, replay or crossed session removes the
bytes; these candidates never enter the utility-review queue. Realtime voice,
voice notes, delegated voice-to-chat and the desktop voice bridge stay
fail-closed.

App processing profiles may only name providers with a reviewed physical
`no_provider_storage` contract. Config validation
(`enforce_app_processing_trust_invariant`) refuses DeepSeek, xAI, Sarvam,
custom adapters and the OpenRouter aggregator; the processing boundary refuses
DeepSeek, xAI and custom adapters with `ProviderRetentionUnsupported` and
OpenRouter with `ProviderBoundaryUnsupported`. xAI keeps an unreviewed per-server prompt cache despite
`store: false`.

**Tools.** Authoring, capability check, publication and VibeDev handoff refuse a
`skill_type: tool` document without a USR contract. Apps declare
`dependencies.tools` by real name; the lock snapshots an eligible reviewed skill
(typed actions required) or typed compiled pack with internal identity
`capability:{name}`. Granting the named tool is the control; attestation is
minted internally. Procedures, personalities, shell bins and raw-argv
passthrough fail closed as tools. A workflow omitting `agent` requests the
default runner `personal-assistant` on the grant. Catalog and review mark
lockable tools without a trusted dispatcher. Dispatch is bind + contain +
receipt, classified from schema, never re-classified from a friendly name at
re-review: [app-tool-bind](app-tool-bind.md).

## Lifecycle and authority

**Authority axes are compared exhaustively.** `compute_permission_diff`
(`magician-apps/src/apps/update.rs`) destructures both grants with no `..`, so a
new `AppGrantRevision` axis does not compile until its review rule is decided.
Network destinations, background execution (going unattended or shortening the
interval) and context reads feed `requires_review`;
`every_authority_axis_expansion_requires_review` covers what the compiler
cannot. Update/reinstall reviews diff the active vs requested grant.
`authorize_update_switch` (`update.rs`) has no callers; commits go through the
approve kernel.
The prior grant resolves via `AppEntityStoreService::data_owner_schema`, **never
`active_schema`** (review runs while `UpdatePending` or `UninstalledRetained`;
purged stays unavailable); read failures propagate as
`AppInstallationReviewError::EntityStore`.

**Owner kill switch.** `POST /apps/installations/{id}/disable`, `/quarantine`,
`/uninstall` (`UninstallRetain`), `/grant-revocations` (revoke + quarantine),
`/update-begin` (parks in `UpdatePending`), `/update-abort` (back to
Enabled/Disabled). Body: closed `{expected_generation, request_id}` (grant
revocation also accepts the UI's legacy empty body, which gets a server one-shot
identity and is not response-loss safe). The client persists exact bytes before
POST; scope, actor, installation, generation, operation and request ID derive
one lifecycle-outbox identity, so response loss replays byte-identically. On
reload the client reposts retained intents (bounded, current scope) and clears
one only after an exact generation/status/review receipt.
`owner_kill_switch_routes_are_wired_to_their_handlers` keeps them reachable.

**Re-enable.** `GET /apps/installations/{id}/reenable-review` shows current
package bytes/lock, grant, schema, compiled surfaces, global policy and physical
implementation digests; `POST /apps/installations/{id}/reenable` echoes the
sealed review digest. The service repeats source/lock review and the registry
repeats every identity in the same IMMEDIATE transaction as the
`ReenableReviewed` CAS. Only an exact Disabled generation with an unrevoked
grant qualifies; memory/retrieval invalidation tombstones stay and old
contributions are not republished. Disable and revoke hide the overlay before
the lifecycle outbox is claimed. The live CLI calls these HTTP owners rather
than writing transitions (a CLI-process registry cannot evict the server's
overlay), as for package export/import and purge preview/commit/status.

## Module inventory

Kernel and execution: `magician/src/magician_v2/apps/`. Route-facing, lifecycle,
surface, purge and projection: `magician-apps/src/apps/`. HTTP:
`magician-api/src/apps_api.rs`.

- Kernel: `models.rs`, `records.rs`, `registry.rs`, `schema_compiler.rs`,
  `entity_store.rs`, `entity_mutation.rs`, `entity_adapter.rs`,
  `package_staging.rs`, `package_transfer.rs`, `authoring.rs`, `lifecycle.rs`,
  `authority.rs`, `boundary.rs`, `approval_boundary.rs`, `policy.rs`,
  `manifest.rs`, `package_lock.rs`, `skill_dependencies.rs`,
  `query_semantics.rs`, `value_mapping.rs`, `artifact_selection.rs`,
  `memory.rs`, `portability.rs`, `resource_contract.rs`,
  `resource_authority.rs`, `workflows.rs`, `llm_operations.rs`,
  `capability_catalog.rs`, `app_tool_bind.rs`.
- `registry.rs` owns scoped SQLite identity, canonical package revision, the
  bounded blocking lane and atomic initial publication; additive migration owns
  generic record/index/receipt/outbox tables (existing scopes migrate lazily;
  absent scopes stay unmaterialized). Standalone procedure revisions are
  write-once bytes with no mutable-skill or global-catalog fallback.
  `authoring.rs` owns `magician app init/check/test/pack` and mints no
  authority.
- Lifecycle/surfaces: `fixtures.rs`, `benchmark_fixtures.rs`,
  `entity_outbox.rs`, `entity_portability.rs`, `entity_retention.rs`,
  `registry_lifecycle.rs`, `threat_model.rs`, `retention.rs`, `update.rs`,
  `sandbox.rs`, `surface_assets.rs`, `surface_host.rs`, `surface_runtime.rs`,
  `surface_worker.rs`.
- Transport consumes only middleware-issued `VerifiedRequestIdentity`, binds the
  transport/session fence, and exposes scoped installation/attempt reads,
  package import/export, bounded owner query/mutation over
  `AppEntityAdapterService`, and the reviewed direct-user action invoke/result
  projection. Server-owned schedule/event launch adapters cannot be minted over
  HTTP; imports are only staged; data bodies cannot carry scope, a store fence
  or mutation provenance.

## Guarantees

### Storage, publication and staging

- `MAGICIAN_ROOT_DIR` (default `$HOME/MagicianNotes`) is pinned to its physical
  directory at boot; registry roots, descendants and SQLite-owned files reject
  symlinks at use. Registry construction touches no filesystem; the first
  authenticated write opens `<scope>/apps/app_store.sqlite3` and binds
  principal/workspace; registry and stager share create-only
  `apps/scope-binding.json`, so a collision fails before rows or bytes exist.
- Initial publication accepts only a non-deserializable value minted from an
  admitted bundle, its immutable lock, a validated revision, a
  conformance-complete attempt and an inert generation-one installation, in one
  `IMMEDIATE` transaction. Exact replay is idempotent; different bytes under a
  reused identity roll back. Reviewable rows grant nothing.
- Portable archive v2's lock is an untrusted claim: publication rebuilds it from
  registry procedure revisions under the authenticated scope. Revision identity
  includes the lock digest; immutable `(package_id, semantic_version)` blocks
  silent dependency replacement.
- Reviewable publication consumes the staged-package proof and revalidates the
  content-addressed directory just before the transaction. Approval publication
  rejoins live scope/actor/session, auth revision and global-policy revision to
  the attempt, package and requested authority. Reviewed activation repeats this
  in an `IMMEDIATE` transaction, validates the grant/schema/surface successor
  set and policy/resource/schema/migration digests, consumes the approval once,
  commits attempt and generation, and appends a metadata-only lifecycle event.
- Enable, disable, update-begin/fail, quarantine, retained uninstall and grant
  revocation are generation CAS (revocation + quarantine in one transaction).
  Outbox claims/acks use private move-only lease tokens with finite limits and
  stale-owner rejection; recovery is explicit, per-scope and bounded, never a
  startup scan. Entity-change projection has its own lease owner (stored digest
  rejoined before delivery).
- Registry access needs a live server-minted, never-deserializable
  `AuthenticatedAppScope` (verified session, or explicit single-user loopback
  checked against the real peer IP; forwarded headers are not evidence). SQLite
  never runs on an async worker; blocking work has a finite admission ceiling;
  writes serialize per scope with a finite wait; background writers have a
  separate ceiling and cannot take the last foreground lane.
- Package staging needs the authenticated scope and a blocking permit, opens
  every component no-follow and descriptor-relative, rejects metadata or
  inventory drift under finite limits, writes to a private sibling, flushes, and
  publishes the bundle-digest directory by create-only rename. A directory-sync
  failure after rename is explicit commit-state-unknown; authenticated recovery
  removes only stale `.staging-*` trees. Nothing runs at startup.

### Manifest, bundle and dependencies

- The procedure loader reads bounded routing metadata first; declared apps are
  validated by the strict parser and excluded from procedure discovery
  (malformed apps cannot fall back). App workflows and private procedures add no
  global procedure/tool descriptors.
- App YAML is a V1 subset: bounded bytes, indentation and lexical depth (compact
  `- - -` nesting and mapping markers count); no aliases, anchors, tags, merge
  keys, block scalars or extra documents; an iterative decoded walk enforces
  node/scalar/depth ceilings; unknown fields rejected.
- Bundle admission takes a complete member inventory and rejects
  unsafe/absolute/traversing paths, compatibility-character escapes,
  non-portable non-ASCII paths, case collisions, links, special files, oversized
  members and missing prompt/asset/vendor references. Production ceilings are
  fixed; readers are capped before allocation; identity covers every path,
  length and digest.
- Dependency locks accept no name-only or self-asserted digests: registry
  evidence is non-deserializable, minted from an exact positive revision and
  computed digest. Vendored identity and `vendor/skills/<name>/SKILL.md` derive
  from the manifest; the digest covers the subtree. Registry-backed skill
  dispatch presents the exact locked revision and bytes; vendored dispatch
  resolves only from the admitted subtree.
- A workflow prompt is read only from its package member. Procedures come from
  registry-minted exact bytes or the rehashed vendored subtree; invocation
  rejoins package, lock and authority and gives each procedure only
  `allowed-tools` ∩ workflow authority (omitted = inherit the narrowed set).
  Prompt/procedure bodies are never serialized or globally cataloged.
- Workflow capabilities need an exact declared dependency and version (no
  implicit `*`). Data-plane production limits have private fields and no
  widening builder.
- V1 views are semantically typed (Table columns exist and are unique; Tree
  bindings have exact field shapes; Timeline needs timestamp/action bindings;
  Graph needs a nullable self-reference parent and a label field; List rejects
  foreign bindings). Routes use a portable ASCII grammar, reject encoded/Unicode
  traversal and reserve `/apps` case-insensitively. Hostile JSON fails a
  byte/depth/node preflight; predicates use a flat arena with iterative
  validation.

### Authority and execution

- Authority resolution checks scope, installation generation and active
  package/grant/schema/surface revisions; disabled, quarantined, retained,
  purged, expired, revoked, malformed or stale inputs fail closed. Tools and
  context reads intersect app grant, acting-agent, trust and optional parent
  ceilings (empty = deny). Classification takes the most restrictive floor;
  every other axis only narrows; a zero resource ceiling denies. The authority
  digest binds actor, session, auth revision, generation, every revision and
  axis, and the grant digest. Digests are `blake3:<64 lowercase hex>`.
- A workflow action is a normal V3 task whose id derives from its invocation
  idempotency identity; its registry control head (not a sidecar) authorizes
  task, run and result state, and ordinary V3 tasks never open it. A committed
  result short-circuits any new execution or admission. Schedule/event sources
  need a short-lived non-deserializable launch proof.
  Delegated/retry/resume/repair records reuse the root binding and budget. A
  terminal commit persists its intent before reservation, revalidates authority
  and permit lifetime just before entity-store I/O, and publishes output only
  after settlement; mutation idempotency is task/output-revision based.
- Model admission binds labels and package/grant/schema authority to one
  endpoint, model, transport cohort, retention posture and continuation
  partition. Protected requests are cold, non-streaming and one-attempt (no
  fallback, retries, reuse, stateful continuation or caches); reviewed adapters
  force storage off and never follow redirects; unsupported/custom providers and
  aggregators without an attested route fail closed. The disclosure window is ≤
  10 minutes (crossing it revalidates source, profile, endpoint and registry
  authority). The LLM permit samples expiry before I/O; missing usage
  observations settle uncertain. Tool egress uses a separate move-only permit
  over authority, labels, destination, live endpoint and exact outbound bytes.
  Results re-enter model continuation only via a server-labeled checkpoint and
  protected-retention permit. Generic memory, task state, reviewer, reflection,
  capture/eval, work-ledger and diagnostics get only lifecycle metadata.
- Data envelopes verify `content_digest` (streamed BLAKE3) against the canonical
  typed value at query-page and action input/output validation. Wire labels and
  policies are claims; joins and consequential consumers accept only trusted
  labels plus an effective policy resolved at the exact final boundary, paired
  with a borrowed revalidated envelope.
- Typed query evidence binds every selected, filtered, ordered and relation
  field; ordering has an implicit record-ID tiebreaker. Server-stored cursor
  evidence binds installation, query/order digests, schema revision, retained
  dataset generation, last record and validity window; clients cannot mint it.
  Selection and continuation each use one SQLite snapshot; continuation keeps
  the original window and is response-loss idempotent. Unrelated edits do not
  invalidate a cursor, but every continuation consumes current authority and
  needs each returned record's exact live revision. Details:
  [app-entity-store](app-entity-store.md).
- Mutation batches are all-or-nothing with an idempotency key; optimistic
  revisions cover every update/delete/restore target; duplicate record/edge
  writes are rejected; relation writes carry both endpoint revisions. Reviewed
  install, update and reinstall need a move-only approval fence bound to
  approval revision, attempt kind, package, authority/policy/schema/migration
  digests, global policy revision and live scope/actor/session/kind/revision;
  durable compare-and-consume owns the approval. Store fences bind the digest of
  the exact validated query or mutation.

### Values and composition

- Workflow values use one content-addressed bounded graph shared by manifest
  admission, Recipe IR, launch/result validation, composition and SDK codecs
  ([app-recipe-ir](app-recipe-ir.md)). Result declarations correlate exact
  typed-value, entity-projection, artifact-reference or receipt-reference roots;
  bridges never substitute task/execution or raw artifact IDs.
- Deterministic composition is a flat bounded select/rename, constant, total
  enum map and registered-conversion program, never code. Compilation rejects
  optional→required, nullable→non-nullable and narrowing enum selects against
  reviewed schemas (not the current value); execution validates the full source,
  omits absent optional mappings and propagates allowed nulls; mapping digest
  and schema identity are exact; unknown output fields are rejected.
- Action-result composition is policy guarded; transfer receipts and digests
  stay sealed in task bindings, and logical transfer identity excludes the
  transient chat execution. A source without a result is `waiting` only while
  its task is nonterminal; failed, cancelled, archived and uncertain sources
  settle as guarded terminal outcomes.

### Contract crate and codegen

The pure `magician-app-contract` crate owns public versions, compatibility,
operation metadata and capability DTOs (`models.rs` keeps unextracted runtime
wire types). `app-contract-codegen` emits Draft 2020-12 schemas, the operation
inventory, supported-public OpenAPI 3.1 paths, a versioned contract manifest,
JSON goldens and TypeScript/Swift fixture catalogs under
`docs/contracts/app-platform/v1`, Unified UI and `magios/Shared`; route
registration consumes the same path/method metadata. `make app-contract-codegen`
regenerates; `make app-contract-check` detects drift and runs Vitest and Swift
round trips.

### Derived content, memory, portability, retention

- Derived content joins trusted classification upward and model permission
  downward, keeps exact source identities and yields deterministic
  policy/provenance/content digests. Locality comes from a current server-owned
  endpoint attestation, never provider or model names; tool egress is an
  independent exact-destination check. Unlabeled bytes are denied to hidden
  consumers; label-blind, analytics and crash diagnostics get metadata;
  model-backed consumers pass the endpoint decision. Provider continuations are
  partitioned by scope, installation, revisions, endpoint/trust/config, model,
  policy, authority and handling.
- App records do not become memory by being written. Candidates come only from
  server-joined content plus exact current record evidence and keep full
  identities and labels; `model_processing: none` or revoked promotion fail
  closed. Proposed candidates are not retrievable; retrieval needs a fresh
  non-deserializable `AppMemoryEligibilityFence` per source. Drift → stale;
  disable/quarantine/update/uninstall-retain → dormant; sole-source delete/purge
  → tombstone; multi-source removal needs a newly reviewed candidate; stale and
  tombstoned never return to accepted. Source evidence binds the same actor,
  session and auth revision as the resolved authority. See
  [app-memory-contribution-storage](app-memory-contribution-storage.md).
- Package and data archives carry no scope, installation, grant, credential,
  schedule, memory or provider-session fields. Data/combined archives default to
  a versioned XChaCha20-Poly1305/Argon2id streaming envelope; plaintext needs a
  separate session-bound warned approval; secret-class data is never eligible.
  The archive writer rechecks live scope, actor, session and auth kind/revision;
  approvals cannot outlive their session.
- A package digest proves bytes only. Signature-chain evidence is
  non-deserializable, byte-bound and accepted only with the current
  trust-registry revision; unsigned imports get a new local-fork identity. Every
  import repeats digest verification, conformance, permission review and
  required rebuild/sandbox qualification; foreign grants never transfer. Data
  import previews cover every record (local IDs, conflicts, rejected fields,
  missing attachments) and bind destination scope, installation ID and
  generation through approval, replay and receipt. Limits apply before sorting
  or hashing.
- Retention has positive time and byte ceilings for revisions, WAL/temp,
  backups/exports, attachments, captures, eval artifacts and audit tombstones,
  plus a per-record revision ceiling; personal, sensitive and secret data need a
  scope-bound key. Purge is a 17-class multi-store protocol (canonical rows,
  WAL/temp, indexes, package/cache, attachments/exports/Artifact V2, memory,
  analytics/captures/evals, provider continuations, routes/surfaces,
  schedules/outbox, disclosure, directory/search), each settling deleted,
  cryptographically erased, shared/policy retained, provider unknown or failed.
  Partial cleanup never becomes `purged`; only the retention owner applies that
  after matching a complete receipt. Storage Governance inventories app storage
  read-only with capped scans and reports partial inventory rather than
  undercounting; it deletes and encrypts nothing.
- Forget protocol V2 persists selection kind plus digest, never raw record IDs.
  Export and import preview share a 10,000-record ceiling; the qualification
  profile (`benchmark_fixtures.rs`) streams 10,000 installation records, 100,000
  scope records and 1,000 outbox rows.

### Resource authority

- Resource journals are stored claims, never authority. Replay needs a fresh
  non-deserializable fence binding scope/session, generation, revisions, root
  execution, ledger and period, minted with a live session; historical replay
  checks tree-local ceilings, not today's concurrency. New root/reservation
  verdicts authorize nothing until appended atomically under the same period
  revision, yielding a move-only permit; exact replay never yields a second
  permit. One shared scheduler gate keeps foreground capacity, prevents two live
  leases per root, and keeps resumed roots on their original period, deadline
  and elapsed-time origin.
- Root, child, retry, resume, repair, tool/browser, synthesis and reflection
  work form one flat bounded tree (iteratively checked, no recursive walk).
  Units use checked arithmetic; parallel active time is an interval union;
  outstanding reservations count their full deadline window; committed intervals
  sit inside their reservation; crash recovery derives windows from the journal,
  not telemetry. Opening retries does not manufacture progress.
- Retention closes periods before compacting terminal trees, writes an immutable
  replay-denial tombstone before deleting a journal, and runs only on the
  background writer lane (≤ 32 trees and 16 MiB per transaction; the keyset
  driver requests eight). Work reserves against committed plus outstanding tree
  and period spend; over-reservation usage is rejected unexposed and held.
  Expiry never releases an uncertain effect; `proven_unspent` needs the
  exclusive crash-reconciler source plus separate recovery evidence.
- **Crash settlement for bound effects.** A crash-recovery owner may commit the
  exact reserved upper bound for a dispatched effect, keeping its effect
  identity, using only the `crash_reconciler` source and no provider-result
  digest or bytes. It cannot claim unspent work, add a physical observer,
  fabricate output or authorize dispatch; crash proof, authority, journal
  revision, reservation bounds and leaf-first closure still apply, and the
  original uncertain event and failed status are preserved. Tests:
  `make test-app-resource-cleanup`.
- Token, MagicLLM task, VibeDev active-time, tool, browser, app-store,
  attachment and package owners are observation sources only; none can mint a
  reservation or answer remaining budget.

## Required memory adopters

`AppMemoryEligibilityFence` is the canonical source-eligibility carrier, not an
app-only side table. Memory owners carry or reference its candidate identity and
fence:

- `magician-vector-index/src/memory_candidates.rs`: persist exact app source
  revisions and handling identity on the candidate;
- `magician-vector-index/src/memory_index.rs`, `memory_hot_projections.rs`:
  exclude candidates without a current fence; invalidate indexed/hot copies on
  source revision or lifecycle change;
- `magician-vector-index/src/memory_temperature.rs`: a utility overlay only;
  cannot restore eligibility;
- `magician/src/magician_v2/agents/memory_consolidator.rs`: nominate and promote
  through the source-linked candidate contract;
- `magician/src/magician_v2/agents/memory_prompt_blocks.rs`: synchronously
  recheck the fence before prompt inclusion.

`magician/src/magician_v2/apps/memory_store.rs` mints current source evidence
from the scoped registry; prompt, search and code-knowledge retrieval compare
the envelope to that live snapshot. Neither an outbox nor a cached serialized
candidate is current-source authority.

## Claims-decision command seam

`magician.claims-decision` is an authoritative command family, separate from the
hypothesis-only contribution terminal. Closed verbs: `confirm_claim`,
`reject_claim`, `record_commitment`, `confirm_commitment`. Typed targets seal
the observed revision (commitments also the full audience address). Proposal,
review and decision bind source provenance, exact display, authenticated scope,
named actor, optional note, disposition and a stable decision id under
domain-separated digests and the trusted desktop signature, which also seals the
active pairing generation (re-pairing stales older envelopes). The proposal uses
a dedicated claims-command header sealing installation generation, package,
grant, schema, workflow, action and the exact `review_decision` source head;
contribution-port, evidence-class, settlement and label fields are absent.

`claims_decision_contribution.rs` accepts only a live server-minted
`AuthenticatedAppScope` whose binding and `actor_ref` match the signed command,
verifies lifetime/signature/contract, reopens pairing generation, declarations,
grant/schema tuple and current source revision, then calls only the
receipt-bearing expected-revision claim and commitment methods (scope and
lifetime resampled under destination locks); transition and receipt share one
JSONL record. Exact retry returns `already_applied`; a changed request under one
decision id or a stale head is refused. `review_decision` and `review_receipt`
entities are projections, never authority. Commitment decisions first bind their
id in a scope-wide index (one id cannot be replayed against another
relationship); that hash-ordered index is not a projection cursor. See
[commitments](commitments.md).

## Meeting-control command seam

`magician.meeting-control` follows the claims shape with its own contract id,
`control_request` source entity and digest labels (neither family validates the
other's envelopes). Verbs `listen`, `join`, `pause`, `resume`, `stop` take
disjoint targets: `new_capture` (optional url/title/date plus explicit
`capture_mic`) for starts, `live_session` (rail-prefixed session id) for
controls.

- Every start carries an `AppMeetingControlGestureV1` (bridge session, unique
  act id, expiry ≤ `APP_MEETING_CONTROL_MAX_GESTURE_AGE_MS`, two minutes) sealed
  into the proposal digest and signed display; risk-reducing verbs must not
  carry one. The gesture is part of the decision id (two acts, two decisions)
  and the destination rechecks freshness on its own clock, so a signed start
  cannot be banked.
- `meeting_control_contribution.rs` reaches capture only via
  `join_meeting_with_scope` and `start_passive_listener` (the `/meetings`
  entries) plus the managers' own pause/resume/stop; a source pin asserts those
  names and forbids `meeting_manager().join(`, `meeting_manager().spawn(`,
  `passive_meeting_manager().spawn(`, `PassiveMeetingSession::new(` and
  `MeetingSession::new(`. A start is refused while any capture is live on either
  rail; every accepted and refused command lands in the shared capture-control
  audit.
- `control_request` (`actor_ref` null at `apply_state: recorded`; the host
  stamps the actor at admission) and `control_receipt` (only from a signed
  destination result) are projections.

Full contract:
[meetings-surface](meetings-surface.md#magicianmeeting-control--the-control-action-class).

## Attention-lane contribution port

`magician.attention.candidate` is the third destination-owned port in
`magician-app-contract/src/contribution.rs`. Apps never own attention state:
they propose lane cards (shared contribution header with one source head,
replace-exact update and tombstone-on-drift; a closed lane matching core
`AttentionLane`; a closed urgency claim, not ranking authority; bounded title
and summary; header `dedupe_key` as idempotency key). The owner decides through
the memory port's desktop-signed, head-bound sealed-proposal shape under
`magician.attention-*` digests, with receipts via `impl_sealed_receipt`. Gated
by the additive `attention_lanes_v1` manifest feature (added in supported-public
contract `1.4.0`; `APP_SUPPORTED_PUBLIC_CONTRACT_VERSION` is now `1.5.0`).
`magician/src/magician_v2/attention_lane_contribution.rs` holds fail-closed
admission (like the memory port's `StageProposal` guard), a domain-separated
idempotency key, and renders an accepted candidate as one `AttentionLaneItem`
(the destination owns ordering: acceptance time, neutral `other` source
kind/family, bounded provenance). Durable staging, owner-review listing and the
dispatch outbox belong to the Channel Assist consumer.

## App LLM operation lane

Named app LLM operations route through the operator-owned
`llm.router.operation_mapping`, with core-lane observability, budgets and
locality policy. Fail-closed at every seam:

| Seam | Owner | Rule |
|---|---|---|
| Manifest | `app.llm_operations`, gated by `llm_operations_v1`; `AppManifestLlmOperation` in `magician-app-contract/src/llm_operations.rs` | Bounded owner-reviewable purpose plus optional narrowing token hint; `manifest.rs` rejects the block without the feature, malformed purposes or zero hints; review re-parses through the same kernel. Declaring nothing keeps the canonical digest. |
| Operator admission | `app_platform.llm_operations` (`magician/src/config.rs`) | Reviewed purpose and positive `max_output_tokens` per name (no default; config load rejects zero; an absent ceiling parses but fails at resolve with `MissingOutputTokenCeiling`). A name absent from policy is rejected for every app. Cap 256 (`APP_LLM_OPERATION_MAX_ADMITTED`) at config load; declarations bounded by `max_collection_items`. Each name needs one `app:<name>` mapping whose `default`, `when_has_images`, `when_cloud` arms name profiles in `app_platform.processing.profiles` and `llm.router.profiles`. |
| Server | `apps/llm_operations.rs` | `app:` keys cannot collide with bare `snake_case` core keys (`AppName` alphabet, no second segment). `admit_manifest_llm_operations` revalidates names against live policy and mapping; `resolve_admitted_app_llm_operation_profile` rechecks live admission (a since-removed name fails closed), resolves locality- and shape-aware through the trusted profile family and returns the live trust declaration. |
| Ceiling | `apps/llm_operations.rs`, `apps/llm_dispatch.rs` | `min(operator ceiling, manifest hint, physical profile, remaining run authority)`: the first three intersect at resolve; `llm_dispatch.rs` clamps to the remaining output-token budget (zero → `CausationBudgetExhausted`). |

Nothing grants blanket LLM access: admitted operations still pass the processing
boundary's disclosure, budget and locality checks and the per-app data policy.
Dispatch rules: [app-behavior-recipes](app-behavior-recipes.md), [background
behaviors](app-background-behaviors-threat-model.md).

## Activated composition and run-control boundary

Query and search return short-lived projection handles bound to actor, scope,
session, execution, source revisions, policy influence, expiry and content
digest: lookup keys, not bearer authority, byte-bounded and quota-partitioned by
scope and session under a global ceiling. Record and action-result composition
reopen both installations, compile the mapping against destination-owned
schemas, persist a destination-correct brokered invocation and call the workflow
owner. An empty present-field fence is valid for constant/control mappings and
never relaxes destination validation.

The move-only owner credential is minted once per authenticated HTTP request
with a server-generated request reference, stable across that request's governed
calls and different per request even if a client reuses a chat-turn id, so
client correlation IDs cannot alias handles. Transfer identity is stable across
physical chat executions; same-key retries resolve to one destination task.
Permanent, transient and effect-uncertain failures are distinct; immutable
task/result/manifest/schema/fence mismatches are corruption; only a legitimate
authority change is stale authority.

The model projection is a minimal DTO without task/execution/transfer IDs,
fences, field sets, source refs, labels or receipt content. A result guard is
integrity-checked and admitted against the actual successful physical profile
and direct-owner audience just before bytes enter a transcript. Chat keeps a
host-only joined policy and lineage; any generic model-visible tool result makes
V1 lineage incomplete, while a typed no-effect app outcome allows an exact
same-source retry; effectful multi-source or unrepresentable derivations are
refused. Realtime/external snapshots, search catalogs, dispatch,
delegated/handover rosters, introspection, autonomous policy snapshots and
tool-search working sets exclude governed app tools via one exposure predicate;
manifest and launch admission reject workflows naming one. Compiled-provider
registration never proves a guard-preserving consumer.

A chat Stop returns a bounded settlement receipt only when the governed provider
returned a typed outcome with its non-serializable result-policy guard;
pre-provider cancellation (including `CompiledDispatchCancelled`) and earlier
refusals are ordinary cancellation; post-boundary dispatch is non-droppable
until its typed outcome is known.

Canonical `task_app_<digest>` lifecycle is owned only by app run control and
exact runtime-outcome settlement; generic task creation, manifest edits,
planning, launch, archive/delete, status override, cancel/delete and
pause/resume/steer fail closed. The innermost runtime boundaries classify both
the reserved namespace and the sealed binding (only a non-canonical task with no
binding is ordinary); namespace guards run before lookup. Artifact V2 exposes
crate-private app seams for creation, launch and delegated parent reactivation;
app terminality projects only from the exact runtime outcome. Delegated
continuation re-enters the workflow owner for fresh admission;
continuation-launch errors settle an unowned runnable parent; forced-child
shutdown commits one cancellation outcome. The Apps UI uses `run_ref` as control
identity, stores only sanitized, scope-partitioned, byte-capped run metadata and
discards malformed status combinations.

Memory-candidate publication is decided inside the per-scope write transaction:
an idempotent replay is recovered before Stop is consulted; only a missing
candidate returns `cancelled`/`effect_committed: false`; new and recovered
proposals report `publication_outcome: published` / `recovered` with
`effect_committed: true` under the source-policy result guard.

## Reconciliation source retention

Native reconciliation derives a source projection from its compiled declaration
before dispatch, keeping the union of source keys and fields used by all targets
bound to that source (`source_document` keeps the whole row). Page membership,
order, cursors, completeness evidence and truncation flags are unchanged. The
raw response must fit its transport ceiling first; the effect owner seals the
compact projection under existing limits; no caller parameter, model call or
re-review is involved. Oversized mapped results fail as a size error (not a
corrupt binding); uncertain dispatch keeps its reservation. Tests:
`make test-app-source-projection`.

## Known gaps

Complete workflow-local reads, remaining public SDK bindings beyond the
manifest-derived authoring types, remaining resource-authority adopters and
threat-matrix rows, and the load-bearing purge/retention/encrypted-stream
adapters in Storage Governance and the archive/crypto owner. Declarative
migrations refuse an unreviewed authority expansion before pointer switch.
