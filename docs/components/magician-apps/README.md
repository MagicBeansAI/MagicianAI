# Magician Apps (runtime surface)

**Current development version:** `0.2.7`

The app-platform **runtime surface** extracted from `magician_v2::apps` as a
satellite crate over the magician lib: surface
hosting/hydration/workers/assets/qualification/runtime, the entity lifecycle
periphery (outbox, portability, retention, changes), `migration`, `sandbox`,
`installation_review` (including governed-MCP contain-profile re-review;
Zepto/Swiggy OS-jail/blocked locks must be republished; INR checkout goes
through `spend_session::admit`), `custom_surface_review`,
`surface_scripted_host`, `threat_model`, `registry_lifecycle`, and
`app_directory`.

## Why only part of apps moved

These files have **zero references** from the magician lib. The rest of `apps`
(workflows, registry, entity_store, surface_compiler, authoring,
package_transfer, …) is locked behind the `execution ↔ apps` cycle.
`magician-api` and `magician-bin` import `magician_apps::apps::…`; the lib's only
reference is a surface-worker child-process test hook via a dev-dependency.

The explicit `magician-apps/test-fixtures` feature forwards to
`magician/test-fixtures` because surface qualification uses the shared manifest
fixture (a dependent crate's test build does not activate this crate's
dev-dependencies). Ordinary builds keep both disabled. Debug binaries must use
the production dependency graph: a build with `magician/test-fixtures` encrypts
Apps databases with a fixture root that production opens reject. Recovery for
such a scope (runtime stopped):
`magician app recover-legacy-fixture-encryption --root ROOT --principal PRINCIPAL --workspace WORKSPACE`,
plus `--commit --backup NEW_FILE` to rekey. It requires the durable OS Keychain
key, authenticates the scope and database integrity, holds an exclusive SQLite
lock, writes a private backup encrypted with the durable key first, refuses
existing backup paths, and never changes records, policies, grants, IDs or
revisions. It is idempotent; the fixture key never enters the normal keyring.

## Generated contract origin

`app_contract_codegen` emits schema `$id` values under
`https://magican.ai/contracts/apps/v1/` (an identifier only; crate paths and API
namespaces remain `magician`).

## Entity store queries and indexes

- Paginated live lists share `AppLiveCollection` in the TypeScript SDK and the
  websocket/visibility driver in Unified UI
  ([collection contract](../magician/typescript-apps-sdk.md#paginated-live-collections)).
- The query owner builds indexed equality/membership snapshots from identifiers,
  revisions and typed sort indexes, fetching payloads only for the requested
  page. Typed timestamp, decimal, integer, null/missing and record-ID ordering
  match the canonical comparator; unsupported predicates use the validated scan
  path. Snapshot capacity, cursor expiry, authority and revision fences still
  apply (`make test-app-indexed-query`).
- An update that changes the compiled index plan takes the compatible migration
  path even with unchanged fields; its identity migration rebuilds derived
  indexes in the destination generation. The update-plan digest and final switch
  bind the destination index plan, so a view-only index change cannot leave rows
  unqueryable.
- Query and terminal tools expose a field catalog from the active compiled
  schema (types, required/nullable, enums, references) with no record values,
  grants or policies, and no extra authority.
- Workflow store reads validate the full typed query against the active schema
  before entity or cursor I/O. An invalid field, predicate, order or relation
  releases its pre-I/O reservation and returns governed failed-call feedback (no
  outcome-uncertain ledger entry). The store transaction still rechecks live
  authority, schema, cursor and record policies; a timeout after dispatch keeps
  conservative settlement.
- The governed `app_store_query` tool derives predicate, ordering and relation
  parameter schemas from the canonical Rust types the store parses, inlining
  nested schemas. `record_id` and `record_revision` are returned row metadata,
  not selectable fields. Failed calls get content-free guidance and are never
  evidence that records are absent.
- The shared terminal tool derives mutation and revision schemas from canonical
  store types: `kind` discriminator, create `payload`/`temporary_id`, update
  `patch`/`record_id`, relation fences, and `revision` copied from the read row's
  `record_revision`. The workflow's mutation ceiling is preserved. Malformed
  arguments rejected during initial decoding (before commit intent or entity
  I/O) get content-free correction feedback; every other terminal failure stops
  the run, and a successful commit ends it without another model call.
- `AppMutationOperation::Create` carries optional `record_id`; fixture and
  hydration builders pass `None` (store mints `rec_<hex>`). Only a caller that
  must address the row by a chosen name supplies one. A field added to a public
  struct variant needs a workspace-wide sweep, including this crate.

## Migration and update review

- Migration writes cite the durable migration run as their mutation receipt;
  the run settles atomically with them. An optional backup is recovery evidence,
  not a prerequisite for a compatible index update.
- If approval publication succeeds but the switch fails, an exact authenticated
  retry reuses the original decision timestamps. A new request after expiry
  creates a new approval revision; the old receipt is never overwritten or
  extended. Workflow launch and contribution publication accept the consumed
  approval revision bound to the committed attempt (a decision revision, not a
  protocol version).
- Update dry-runs compile the destination with the policy from the server's
  installation review; the source grant still governs current data access. The
  plan digest seals the compiled destination schema including field policies, so
  a processing-policy update cannot reuse a preview compiled under the old grant.
  Activation requires an exact match with the reviewed schema and a current
  source fence.
- Review discovery filters retained attempts against the installation's current
  update or reinstall source fence. Cancelling and restarting keeps evidence but
  requires a new review attempt; an old `ready_for_review` row cannot block the
  seed publisher.
- A new update attempt may reuse a published package revision; publication
  preserves its creation timestamp after comparing every other immutable field.
- **Remote processing for existing records.** Changing a package or grant to
  `remote_allowed` does not release restrictions on existing records. An owner
  may include
  `{"kind":"enable_remote_processing_for_existing_records","entity":"turn_cursor"}`
  in an update plan: it changes only the selected entities' `local_only` record
  policies to `remote_allowed` (`none` policies, payloads, classification,
  memory, personal-agent and egress permissions untouched; compiled field pins
  still restrict reads). The destination must grant remote processing, entities
  must exist on both sides, and a nonempty source needs an encrypted pre-update
  backup before exact owner approval (an empty source rechecks the fence and zero
  record heads instead). Plan digest, staged revisions and change sequences are
  rechecked at the atomic switch. Code-only updates or inferred migrations never
  do this implicitly.

## Primitive binding drift

`authorize_locked_primitive` compares a package's locked primitive binding with
the descriptor from the live authoring catalog. When a platform descriptor is
regenerated its action identities change and locked selectors no longer
resolve; `AppLockedPrimitiveBinding::matches_descriptor` then returns
`Ok(false)` (other resolver errors still propagate), so the caller raises
`PrimitiveBindingMismatch`. Review treats that like a legacy name-only lock: a
non-dispatchable review item ("locked primitive binding no longer matches the
live descriptor; requires owner-approved republish and installation re-review").
The owner can still approve; the drifted tool stays disabled until republish.
Why: a platform change must not make a correct package unreviewable or blame its
author.

## Native surfacing distribution boundary

Widget, indicator, slot-suggestion, distribution and navigation declarations are
review input, not runtime provenance. Ordinary staging, SDK and VibeDev
publication reject `distribution: system`; only host-controlled digest-pinned
boot admission publishes system packages. Native installable widgets compile
lazily only after exact enabled installation/package/generation revalidation.

An indicator compiles only with an exact selector; without one it compiles to
nothing rather than an unfiltered one-row read (meetings' chip narrows to the
live `capture_session` row; Town Square's names the `policy` singleton). Native
widget declarations pair with `fallback: unavailable`, because a `view` fallback
would need a second declared read the V1 compiler refuses.

## Background behaviors

`app_behaviors_v1` is an additive manifest/review vocabulary whose runtime is
boot-gated off by default (`app_platform.background_behaviors.enabled`). When
enabled, the scheduler executes deterministic `Recipe` workflows without model
operations, or `Auto` workflows with an explicit reviewed operation recipe; an
unordered allow-set without steps stays blocked. Scheduled and event lanes share
one readiness check; workflow admission still checks exact grants and budget
before model I/O.

- **Declaration.** Each behavior binds one unique scheduled action (workflow
  `trigger: schedule`), a bounded interval cadence, a bounded purpose, the
  complete reviewed LLM-operation allow-set, an optional closed structured-output
  schema, and one exact entry under `app.resources.behaviors`. Its mandatory
  input is one closed singleton selector `{entity, record_id, fields}`: fields
  non-empty, unique, declared by the package-owned entity, and exactly equal to
  the workflow input schema. No predicate, first-row, ambient query or
  empty-object fallback.
- **Review.** Installation review derives a digest-bound request per behavior,
  including exact purpose text. The owner may deny behaviors, slow cadence, or
  lower per-run, monthly, start-rate, causation, spend-depth and
  contribution-proposal ceilings, but cannot substitute action, operation set,
  output-schema digest, period or review digest. The selector's canonical digest
  is frozen into requested and granted entries, which persist on
  `AppGrantRevision`, join the authority digest only when non-empty, and are an
  independent update expansion axis.
- **Recipe seal.** `reviewed_behavior_grants` / `reviewed_event_behavior_grants`
  compute `steps_digest` from the ordered recipe and fold it into
  `app_behavior_request_digest` (`None` for step-less behaviors, keeping their
  earlier digest), so a recipe cannot be reordered under an unchanged grant. See
  [app-behavior-recipes](../magician/app-behavior-recipes.md).
- **Accounting.** Scheduler-minted runs seal a content-free behavior resource
  identity into their run/root binding. Monthly admission checks both the
  installation-wide total and an indexed total per behavior digest, so behaviors
  cannot consume each other's allowance or collectively bypass the app ceiling.
  Ordinary app tasks omit the discriminator.
- **Disclosure.** Before each model admission the workflow owner reopens the
  reviewed singleton selector in one SQLite snapshot and checks content, fields,
  record revision and source policy against the sealed task; changed, missing or
  foreign records are refused, and no envelope or provenance reference can grant
  model access. Warnings expose a content-free reason code. Scheduled and event
  inputs seal the complete source-and-destination handling policy at launch, and
  execution verifies that same input-policy digest (not the destination grant
  digest).
- **Dispatch.** Reviewed steps run through the shared LLM dispatcher, each call
  bound to one declared operation and the exact behavior, task, execution and
  scope, on the `App(app:<name>)` scheduled workload with a pinned attested
  profile and the minimum of operator, manifest, profile and remaining-run output
  ceilings. Registry, grant, resource and operation-policy guards apply to every
  attempt. Zero-operation behaviors are valid for deterministic recipes and cannot
  declare `output_schema`.
- **Structured output.** Model steps derive a strict transport schema from the
  reviewed output schema: every property required on the wire, null standing for
  an omitted optional non-nullable field. The decoder removes only those
  transport nulls, preserves explicitly nullable fields, then validates the
  original schema (nested records, arrays and tagged unions included).
- **Registry state (schema V25+).** Separate partial due indexes for idle and
  expired-pending heads, closed diagnostic codes, payload-free retry evidence,
  resumable reconciliation progress, incomplete-visit debt, a due-work fairness
  cursor, and bounded events beside the behavior-local ledger binding. Runs
  accepted by the pre-discriminator V22 schema keep `None` identity: never
  upgraded or treated as current authority; only a cleanup-only seam may settle
  already-dispatched work and close terminal nodes.
- `ReviewedBackgroundLaunch` is an execution-only authentication class for an
  exact validated background launch; installation review rejects it and it
  cannot consume an owner approval.

## Model turns and effects

- Protected Apps model turns rebuild the full prompt and replay bounded history
  every call and send no provider continuation ID, because Apps admission
  requires no provider storage.
- Governed model tool calls bind their App invocation, resource reservation and
  retained result to the runtime's validated per-call effect ID, so several
  calls in one turn get distinct reservations; replay keeps identity and changed
  arguments fail the authority's identity check. Missing attribution fails before
  dispatch.
- App effect admission uses the resource reservation's absolute deadline from
  durable root admission (no separate preflight clock, no renewal). A typed
  admission expiry after durable abort-before-I/O settlement permits one fresh
  attempt with the same arguments; cancellation, uncertain settlement and plain
  error text do not.
- App workflows commit typed results and mutation receipts through the Apps
  owner; their internal task shells finish without generic chat outputs or a
  second synthesis call. The progress publisher checks the durable workflow task
  binding (not IDs or labels) before warning about missing outputs; unknown
  ownership keeps the warning.
- Terminal recovery distinguishes missing execution records from completed task
  deletion: only a verified durable deletion marker (exact loop scope, segment
  and receipt, no surviving task directory, under the task transaction lock) can
  suppress the terminal outbox batch; otherwise recovery debt stays visible.

## Registry concurrency

- The registry pool (eight operations) limits simultaneous blocking database
  operations, not installed Apps or waiting workflows. Reads and foreground
  writes queue until admitted, cancelled by the caller, or shut down; there is no
  registry-local admission deadline. Writer admission acquires a blocking slot
  and scope lock as a pair, never holding one while awaiting the other, and
  keeps both through SQLite completion, including caller cancellation.
  Background maintenance keeps a separate foreground reserve.
- The registry reuses authenticated SQLCipher connections (at most 32 idle
  handles across scopes, separate read-only and writable). Checkout checks file
  identity, scope binding, encryption generation and schema version; full schema
  validation runs on cold opens and schema-cookie changes. Grants and
  installation revisions remain live checks. Unfinished transactions and suspect
  handles are discarded; key rotation drains the database's idle handles.
  Increasing the pool alone is not evidence of scalability.
- Profiling: completed leases record checkout time, operation time, SQL
  statement counts/duration and explicit transaction counts/duration per
  SQLCipher connection. Enable `magician::apps::registry=debug` for per-lease
  events and admission outcomes (`admitted`, `closed`, `cancelled`). SQL,
  parameters, keys and contents are never recorded. `connection_pool_stats()`
  exposes process-aggregate counters (not a scoped capability).
- Page slot reads load assignments once and initialize only referenced
  installations via the widget runtime's targeted inventory seam; Settings and
  boot request the full inventory. Widget render batches reopen installation
  records in one short read transaction.
- The package stager caches immutable admitted member bytes (≤ 64 packages /
  64 MiB). Each hit reopens the scoped package tree, checks its
  descriptor-pinned inventory (ctime, ownership, links, inode) and exact index
  bytes; any change invalidates reuse. Eviction triggers fresh admission and
  never limits installed Apps. Concurrent cold reads coalesce. Installation,
  grant and schema authority are never served from this cache.
- Recipe locks bind whole host implementation files (including the canonical
  task service), so changed covered bytes require a new reviewed lock even for
  compatible behavior. Do not remove identity checks or silently relock grants to
  avoid that.

## Contextual round progression

`contextual_round` is a pure progression component for independently settled
participants. It freezes permitted per-participant context, applies owner
exclusions, rotates in stable identity order, and keeps draft, committed, quiet,
failed and deferred states distinct. Retries yield to participants not yet
attempted. Usage includes settled attempts and outstanding reservations; restart
and cancellation never refund unknown consumption. Prepared drafts keep a stable
mutation key so receipt recovery needs no second model call. Cursor progress
follows attempted participants so a limited budget does not favor leading
entries.

It authorizes nothing; the native recipe owner connects context reads, reviewed
operations, dispatcher and resource ownership, checkpoints and mutation receipts.
`context_mode: progressive` seals fresh permitted context before each first
attempt (kept across retries) for sequential speakers; an unsettled predecessor
cannot become evidence. The default snapshot mode is concurrent.

## Experience classes

`installation_review.rs` revalidates the admitted experience classes
(`overlay-draw`, `narration`) by recomputing schema digests, ceilings and
implementation-plan digests like the browser arm, reachable only for a
dispatch-Ready descriptor (the admitted descriptors are `Conditional`, so both
fail closed). `threat_model.rs` carries the six red cases, including R2
`app-voice-invocation-phrases-stay-unadmitted`. The admission surface lives in
`magician_v2::apps::experience_capability`; see
[app-interactive-capabilities.md](../magician/app-interactive-capabilities.md).

## Custom-surface review and scripted host

`custom_surface_review.rs` owns owner review for `custom_surfaces_v1`: it
hydrates declared entry points and the full executable-member inventory (with
digests) from the staged package, runs the static asset scan (an aid, never a
boundary), pins the `allow-scripts` sandbox and deny-egress CSP constants, and
validates per-entry-point narrowing (omitted or empty grants no surfaces).
`installation_review.rs` folds it into the review-material digest, approve
receipt and grant revision:
`AppGrantRevision.granted_custom_surface_entry_points` persists the attested
`(route, document, digest)` set, joins the authority digest when non-empty, and
appears in `compute_permission_diff` as `custom_surface_entry_points`.

`surface_scripted_host.rs` compiles the host plan (kernel-constant
`allow-scripts` sandbox, single-host-origin deny-egress CSP, digest-keyed entry
address) and serves only digest-verified `surfaces/` bytes from the exact live
revision.

- `compile_scripted_surface_host_plan` admits only an exactly granted
  `(route, document)` whose live entry document still hashes to the granted
  digest (`EntryPointNotGranted` / `GrantedDigestStale`; an empty grant hosts
  nothing).
- The minted `entry_url` carries the live session reference as its first asset
  path segment, because a sandboxed frame's opaque origin can send no headers.
  Relative subresources resolve by ordinary URL resolution under the session's
  still-live entry-document digest; `serve_asset` serves the sibling's own
  manifest-verified bytes with `no-store`. A drifted entry document refuses its
  siblings as it refuses itself; script-capable non-entry documents and wasm are
  refused; wrong digests are `DigestMismatch`.
- The session carries `AppScriptedSurfaceRequestScope` (principal + workspace)
  from `open_host`, lent back by `session_scope` only while the session lives and
  never serialized. The asset route uses it as fallback when a credential-less
  frame GET (Cloudflare Access identity, no `X-Workspace`) would otherwise answer
  `app_workspace_required`; the bridge POST keeps the full envelope contract.
- Bridge methods are exactly the eight supported-public operations
  (`AppSurfaceV1Method` → `AppPublicOperationId`).
- Budgets: 15-minute TTL, 32 messages and 256 KiB payload per session, 8
  sessions per installation, 3-strike reload/crash budget into quarantine. TTL is
  re-checked on every path that lends a session (`serve_asset`, `session_scope`,
  `note_reload`, which also checks the installation binding); expired entries are
  evicted on touch, and `open_host` sweeps expired entries before counting the
  per-installation budget so idle opens cannot wedge it.
- Teardown maps onto `AppCustomSurfaceTeardown` and the frame is replaced by the
  "failed safely" notice.

`threat_model.rs` carries twelve custom-surface red rows (T1–T12): T1–T4, T6 and
T8 have kernel tests in `surface_scripted_host.rs`; T5 is covered in
`magios/MagiosTests/AppSurfaceScriptedHostTests.swift`; T7 is the owner-ratified
residual; T9–T11 are `DormantBoundarySpecified`. Manifest, operator switch and
reference consumer (`magician_data_v3/system/thinking_map/app`):
[custom-surfaces-v1.md](../magician/custom-surfaces-v1.md).

`app_directory.rs` entries carry `custom_surface_entry_count` (captured into
`AppPackageDirectoryMetadata` at admission; always emitted on the wire, capped
at eight). iOS gates "Open app" on a count > 0; unified-ui normalizes absence to
0 and still disables Open via `default_route`.

## Review: memory, secrets, hosts

- **Memory read** (`app_memory_read_v1`). `AppInstallationReview` carries
  `requested_memory_read` (request, digest, sensitive tiers, default grant),
  folded into the review digest only when present.
  `AppInstallationApproveRequest::granted_memory_read` must echo the request
  digest and stay within it; omitted means the default (non-sensitive tiers and
  agents while the owner uses the app, nothing in background). A choice for an
  app that requested nothing is refused. `AppPermissionDiff::memory_read`
  treats any new tier or agent in either mode as an expansion. Purge removes
  `app_memory_read_grant_heads`. See [app-memory-access](../magician/app-memory-access.md).
- **Secret use.** `requested_secret_uses` lists (tool, secret, required,
  destination) from each locked OS-jail tool's reviewed source
  (`secret_access::app_secret_use_requests`), present and digested only when a
  tool uses a secret. Requests also carry `destinations`, `app_granted_hosts`
  and `delivery: config_file`. `granted_secret_uses` must be a unique subset;
  omitted grants nothing. `AppPermissionDiff.secret_uses` marks a new pair, an
  added host, or "any site" as `expanded`; fewer hosts is `narrowed`.
- **Key scopes.** A key of a tool that declares no host is granted with
  `AppSecretUseGrant::hosts` (one or more of the app's named hosts) or `any_site`
  (only when the app asks for any public host); a tool declaring `*` only with
  `any_site`; a declared tool's key takes neither (`validate_secret_use_grant`;
  impossible choices carry `not_grantable`). Stored pre-scope grants with an
  unscoped key still validate (`validate_stored_secret_use_grant`), but the call
  treats the key as not granted and re-approval drops it
  (`without_unscoped_legacy_grants`).
- **Tool runtime.** `tool_runtime` has one `AppReviewedToolRuntime` per locked
  OS-jail tool that runs in place or declares hosts: `in_place_from`,
  `declared_hosts`, `reaches_granted_hosts`, and `reachable_hosts` (declared hosts
  the app names, or every app-named host when it declares none). Built by
  `os_jail::reviewed_os_jail_runtime`.
- **Any public host.** Offered (`offers_any_public_host`) only when the manifest
  data policy asks (`external_egress: any_public_host`) and a locked in-place
  tool declares no host. Approval takes `granted_any_public_host` (default
  false; refused unless offered), stored on `AppGrantRevision` and folded into
  the authority digest only when set; the diff reports gaining it as `expanded`.
- New review/diff axes are skipped when empty or unchanged and join digests only
  when present, so existing installs and older diffs keep their digests.
- Revalidation passes each skill's directory to
  `revalidate_locked_os_jail_artifact`; an in-place tool that cannot run shows
  the exact reason, and a changed skill asks for re-approval. See
  [app-os-jail-egress](../magician/app-os-jail-egress.md#in-place-skills-inplaceskill-app_in_place_skill_v1),
  [secrets](../magician/app-os-jail-egress.md#secrets-api-key-skills-app_secret_use_v1),
  [granting any host](../magician/app-os-jail-egress.md#granting-it-app-owner).

## Older-data maintenance

`AppEntityRetentionService` owns age cleanup for every installation with indexed
timestamp fields: preview, confirmation, durable batched progress and reference
protection through the normal registry and erasure owners, with no package update
or LLM call. It is host-owner maintenance, absent from the sandbox bridge and
workflow tool grants; purge removes its job/candidate metadata. See
[the entity-store contract](../magician/app-entity-store.md#owner-approved-age-cleanup).

## System seed packages

Colocated system-class seeds live at `magician_data_v3/system/*/app/`
(`claims_review`, `thinking_map`, `meetings`, `town_square`, learning), beside
the subsystem each renders. The docs-guard ownership rule matches that glob.

- **Generated artifacts.** Every seed carries `.magician/app-derived.json` and
  `sdk/app.generated.ts` from `magician app check --write-generated`.
  Publication refuses missing or stale artifacts, and the bundle digest covers
  them; regenerate after any manifest change.
- **Dependency locks.** Package versions seal their dependency locks. When an
  embedded primitive binding changes, bump every affected seed and regenerate,
  even with unchanged workflows; reusing a version keeps the old lock by design.
  New seed revisions go through normal update review and never silently replace
  grants.
- **Boot replay** recognizes an enabled or disabled system package at the
  shipped revision through its retained installation and committed attempt;
  in-place updates keep the installation ID. An installation in `UpdatePending`
  with no ready review attempt is repaired at boot by publishing the trusted seed
  through the fenced update publisher (keeps ID, records and grant; stays
  pending until reviewed).
- **Budgets.** Foreground runs reserve up to 131,072 tokens (Town Square
  262,144) and 600 active seconds; the workflow profile reserves its full
  32,768-token input window before each call. Monthly: 400,000 (Brainstorm,
  Learning), 600,000 (Claims), 900,000 (Meetings), 4,000,000 (Town Square).
  Town Square's ambient behavior: 262,144 tokens, $0.25 and 600 active seconds
  per run, one start per hour, 1.8M tokens / $46 per month.
- **Meetings** pairs six `meetings_data` read-binder actions with five capture
  controls that are not tool actions: a control workflow writes one immutable
  `control_request` row, and only the reviewed `magician.meeting-control`
  destination applies it through the same entries `/meetings` uses. Its
  `surfaces/console.js` spends the 32-message session budget from a bounded pool
  with a reserve so STOP is never blocked, stretches its interval as the pool
  drains, and shows the remaining count. See [meetings-surface.md](../magician/meetings-surface.md).
- **Town Square** declares `behaviors`, `llm_operations` and workflow
  `notification_ports`, and its entity store is the **authority** for its corpus,
  so disabling the package does not preserve a host substrate; stop autonomous
  posting with `policy.autonomy_state`. Console rules: the write reserve is
  metered against the poll budget (reaction controls must not drain it), and the
  policy form re-seeds from a poll only while untouched. Setup is detected from
  the `turn_cursor` row (not `policy`); when missing the console launches
  `sync_roster` once and reports it as launched. Its scheduled gate and compose
  operations run before corpus reads and get no persona or recent feed, so a
  completed empty-draft turn may only advance the cursor; completion is not
  proof of a post. See [town-square-app.md](../magician/town-square-app.md).
- **Memory Learning** `sync_queue` uses the deterministic reconciliation recipe:
  reviewed inputs forward only `limit` and `state` to
  `internal_data.list_learning_candidates`, preserve nullable fields, IDs and
  records outside the page, and complete through the terminal transaction
  without model generation.
- **Claims Review** aligns its view with the native `/claims-review` workspace,
  adds a read-only register sync that atomically projects a bounded source page
  and its continuation cursor, and preserves full statement wording
  ([Claims Review](../unified-ui/claims-review.md#installed-app)).

## Tests

- `make test-app-reconciliation` — reconciliation, effect admission, registry
  contention (Tokio controlled clock).
- `make test-app-registry-admission` — registry admission and profiling against
  real SQLCipher, without recompiling the monolith.
- `make test-app-indexed-query` — production SQL planner (10,000 records).
- `make test-app-contextual-round` — progression, context refresh, recovery and
  the shipped recipe.
- A schema-35 fixture regression verifies upgrade through schema 37 while
  retaining records.
