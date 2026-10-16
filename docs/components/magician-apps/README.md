# Magician Apps (runtime surface)

**Current development version:** `0.2.8`

The app-platform **runtime surface**, extracted from `magician_v2::apps` as a
satellite crate over the magician lib: surface
hosting/hydration/workers/assets/qualification/runtime, the entity lifecycle
periphery (outbox, portability, retention, changes), `migration`, `sandbox`,
`installation_review` (including governed-MCP contain-profile re-review;
Zepto/Swiggy OS-jail/blocked locks must be republished; INR checkout goes
through `spend_session::admit`), `custom_surface_review`,
`surface_scripted_host`, `threat_model`, `registry_lifecycle` and
`app_directory`.

## Why only part of apps moved

These files have **zero references** from the magician lib; the rest of `apps`
(workflows, registry, entity_store, surface_compiler, authoring,
package_transfer, …) is locked behind the `execution ↔ apps` cycle.
`magician-api` and `magician-bin` import `magician_apps::apps::…`; the lib only
has a surface-worker test hook via a dev-dependency.

`magician-apps/test-fixtures` forwards to `magician/test-fixtures` (surface
qualification uses the shared manifest fixture, and a dependent crate's test
build does not activate this crate's dev-dependencies); ordinary builds keep
both off. Debug binaries must use the production graph: `magician/test-fixtures`
encrypts Apps databases with a fixture root production opens reject. Recovery
(runtime stopped):
`magician app recover-legacy-fixture-encryption --root ROOT --principal PRINCIPAL --workspace WORKSPACE`,
plus `--commit --backup NEW_FILE` to rekey. It needs the durable OS Keychain
key, authenticates scope and database integrity, holds an exclusive SQLite
lock, writes a private backup under the durable key first, refuses existing backup paths, never
changes records, policies, grants, IDs or revisions, and is idempotent; the
fixture key never enters the normal keyring.

## Generated contract origin

`app_contract_codegen` emits schema `$id` values under
`https://magican.ai/contracts/apps/v1/` (an identifier only; crate paths and
API namespaces remain `magician`).

## Entity store queries and indexes

- Paginated live lists share `AppLiveCollection` (TypeScript SDK) and the
  Unified UI websocket/visibility driver
  ([collection contract](../magician/typescript-apps-sdk.md#paginated-live-collections)).
- Indexed equality/membership snapshots come from IDs, revisions and typed
  sort indexes; payloads load only for the requested page. Typed timestamp,
  decimal, integer, null/missing and record-ID ordering match the canonical
  comparator; other predicates use the validated scan path.
  Capacity, cursor expiry, authority and revision fences apply
  (`make test-app-indexed-query`).
- A changed compiled index plan forces the compatible migration path even
  with unchanged fields; its identity migration rebuilds derived indexes in
  the destination generation; the update-plan digest and switch bind the
  destination plan, so view-only index changes cannot leave rows unqueryable.
- Query and terminal tools expose a field catalog from the active compiled
  schema (types, nullability, enums, references) with no values, grants,
  policies or authority.
- Workflow store reads validate the typed query before entity or cursor I/O;
  an invalid one releases its reservation with governed failed-call feedback
  (no outcome-uncertain ledger entry). The transaction rechecks authority,
  schema, cursor and record policies; a post-dispatch timeout keeps
  conservative settlement.
- `app_store_query` derives parameter schemas from the store's canonical Rust
  types; `record_id` and `record_revision` are returned metadata, not
  selectable. Failures get content-free guidance and never prove absence.
- The terminal tool's mutation schemas come from canonical store types (`kind`
  discriminator, create `payload`/`temporary_id`, update `patch`/`record_id`,
  relation fences, `revision` from the row's `record_revision`), under the
  mutation ceiling. Malformed arguments rejected before commit intent get
  content-free correction; other terminal failures stop the run; a successful
  commit ends it without another model call.
- `AppMutationOperation::Create` carries optional `record_id` (fixture and
  hydration builders pass `None`; the store mints `rec_<hex>`); only a caller
  addressing the row by a chosen name supplies one. New fields on a
  public struct variant need a workspace-wide sweep, this crate included.

## Migration and update review

- Migration writes cite the durable migration run as receipt and settle
  atomically with it; a backup is not required for a compatible index update.
- If approval publishes but the switch fails, an exact authenticated retry
  reuses the decision timestamps; after expiry a new approval revision is
  created (old receipts never overwritten or extended). Workflow launch and
  contribution publication accept the consumed approval revision bound to the committed attempt.
- Dry-runs compile the destination under the server review's policy (the
  source grant governs current data); the plan digest seals the schema with
  field policies (a processing-policy update cannot reuse a preview compiled
  under the old grant); activation needs an exact match and a current source
  fence.
- Review discovery filters attempts by the current update/reinstall source
  fence; cancel-and-restart keeps evidence but needs a new attempt, and a stale `ready_for_review` row cannot block the seed
  publisher.
- A new attempt may reuse a published revision; publication keeps its
  creation timestamp after every other immutable field matches.
- **Remote processing for existing records.** Switching to `remote_allowed`
  never releases existing records. An owner may add
  `{"kind":"enable_remote_processing_for_existing_records","entity":"turn_cursor"}`
  to an update plan: only those entities' `local_only` record policies become
  `remote_allowed` (`none` policies, payloads, classification, memory,
  personal-agent and egress untouched; field pins still apply). The
  destination must grant remote processing, entities must exist on both sides,
  and a nonempty source needs an encrypted pre-update backup before exact
  owner approval (an empty source rechecks the fence and zero record heads). Digest, staged revisions and change sequences are rechecked at the
  switch. Code-only updates and inferred migrations never do this.

## Primitive binding drift

`authorize_locked_primitive` compares a package's locked primitive binding
with the live authoring catalog descriptor. After a platform regenerates
descriptors, `AppLockedPrimitiveBinding::matches_descriptor` returns
`Ok(false)` (other resolver errors propagate) and the caller raises
`PrimitiveBindingMismatch`. Review treats it like a legacy name-only lock: a
non-dispatchable item ("locked primitive binding no longer matches the live
descriptor; requires owner-approved republish and installation re-review").
The owner can still approve; the drifted tool stays disabled until republish.

## Native surfacing distribution boundary

Surfacing declarations (widget, indicator, slot-suggestion, distribution,
navigation) are review input, not provenance. Staging, SDK and VibeDev publication reject
`distribution: system`; only host-controlled digest-pinned boot admission
publishes system packages. Native widgets compile lazily after exact enabled
installation/package/generation revalidation. An indicator without an exact
selector compiles to nothing, never an unfiltered read (meetings' chip: the
live `capture_session` row; Town Square: the `policy` singleton). Native widget
declarations pair with `fallback: unavailable` (a `view` fallback needs a
second read the V1 compiler refuses).

## Background behaviors

`app_behaviors_v1` is an additive manifest/review vocabulary, runtime
boot-gated by `app_platform.background_behaviors.enabled` (off when the key is
absent; the repo-root `magician-config.yaml` seed sets it `true`). When on, the scheduler runs deterministic `Recipe` workflows or `Auto` workflows
with a reviewed operation recipe; a step-less allow-set stays blocked.
Scheduled and event lanes share one readiness check; admission checks exact
grants and budget before model I/O.

- **Declaration.** One unique scheduled action (`trigger: schedule`), bounded
  interval and purpose, complete LLM-operation allow-set, optional closed
  output schema, one `app.resources.behaviors` entry. Input is a closed
  singleton selector `{entity, record_id, fields}` (fields non-empty, unique,
  declared by the package-owned entity) exactly matching the workflow input
  schema; no predicate, first-row, ambient or empty fallback.
- **Review.** A digest-bound request per behavior (purpose included). The
  owner may deny, slow cadence or lower per-run, monthly, start-rate,
  causation, spend-depth and contribution ceilings, but not change action,
  operations, output-schema digest, period or review digest. The selector
  digest persists on `AppGrantRevision`, joins the authority digest only when
  non-empty, and is its own update expansion axis.
- **Recipe seal.** `reviewed_behavior_grants` /
  `reviewed_event_behavior_grants` fold `steps_digest` into
  `app_behavior_request_digest` (`None` when step-less), so recipes cannot be
  reordered under an old grant
  ([app-behavior-recipes](../magician/app-behavior-recipes.md)).
- **Accounting.** Scheduler-minted runs seal a content-free behavior identity;
  monthly admission checks the installation total and an indexed per-behavior
  total, so behaviors cannot eat each other's allowance or jointly bypass the
  app ceiling. Ordinary app tasks omit the discriminator.
- **Disclosure.** Before each model admission the selector is reopened in one
  SQLite snapshot and checked (content, fields, revision, source policy)
  against the sealed task; changed, missing or foreign records are refused
  with a content-free reason code, and no envelope or provenance reference can
  grant model access. Inputs seal the full source-and-destination handling
  policy at launch; execution verifies that input-policy digest (not the
  destination grant digest).
- **Dispatch.** Steps use the shared LLM dispatcher, bound to one operation and
  the exact behavior, task and scope, on the `App(app:<name>)` workload with a
  pinned attested profile and the minimum of operator, manifest, profile and
  remaining-run output ceilings. Registry, grant, resource and
  operation-policy guards apply to every attempt. Zero-operation behaviors
  (deterministic recipes) cannot declare `output_schema`.
- **Structured output.** A strict all-required transport schema; the decoder
  strips only transport nulls for omitted optionals, keeps explicitly nullable
  fields, and validates the original.
- **Registry state (schema V25+).** Partial due indexes, closed diagnostic
  codes, payload-free retry evidence, resumable reconciliation,
  incomplete-visit debt, a fairness cursor and bounded events. V22-accepted runs keep `None` identity and are
  never upgraded; only a cleanup-only seam settles dispatched work.
- `ReviewedBackgroundLaunch` is execution-only; review rejects it and it cannot
  consume an owner approval.

## Model turns and effects

- Protected Apps model turns rebuild prompt and bounded history each call (no
  provider continuation ID or storage).
- Governed tool calls bind invocation, reservation and retained result to the
  per-call effect ID (several calls in a turn get distinct reservations);
  replay keeps identity, changed arguments fail, missing attribution fails
  before dispatch.
- Effect admission uses the reservation's absolute deadline (no renewal); only
  a typed expiry after durable abort-before-I/O allows one fresh attempt with
  the same arguments; cancellation, uncertain settlement and plain error text
  do not.
- App workflows commit results through the Apps owner (no chat outputs or
  second synthesis call); the progress publisher checks the durable workflow
  task binding (not IDs or labels), so missing-output warnings fire only for
  unknown ownership.
- Only a verified durable deletion marker (exact scope, segment, receipt, no
  surviving task directory, under the task lock) suppresses the terminal outbox batch; else recovery debt shows.

## Registry concurrency

- The registry pool (eight operations) bounds blocking database work, not
  installed Apps. Waiters queue until admitted, cancelled or shut down (no
  registry deadline). Writers take a slot and scope lock as a pair (never
  holding one while awaiting the other) through SQLite completion, caller
  cancellation included; background maintenance keeps a foreground reserve.
- Authenticated SQLCipher connections are reused (≤ 32 idle, separate
  read-only and writable); checkout checks file identity, scope, encryption
  generation and schema version, with full validation on cold opens and
  schema-cookie changes. Grants and installation revisions stay live checks;
  unfinished transactions and suspect handles are discarded; key rotation
  drains idle handles.
- Leases record checkout/operation time and SQL statement and transaction
  counts/durations; `magician::apps::registry=debug` logs them with outcomes
  (`admitted`, `closed`, `cancelled`), never SQL, parameters, keys or
  contents. `connection_pool_stats()` exposes process-wide counters (not a
  scoped capability).
- Page slot reads load assignments once and initialize only referenced
  installations (targeted inventory seam); Settings and boot request the full
  inventory. Widget render batches reopen installations in one short read
  transaction.
- The package stager caches admitted bytes (≤ 64 packages / 64 MiB), rechecking
  inventory (ctime, ownership, links, inode) and index bytes per hit (any
  change invalidates reuse; eviction means fresh admission, never a cap on
  installed Apps; cold reads coalesce); never installation, grant or schema
  authority.
- Recipe locks bind whole host files (including the canonical task service):
  changed bytes need a new reviewed lock even for compatible behavior; never
  drop identity checks or silently relock grants.

## Contextual round progression

`contextual_round` is a pure progression component for independently settled
participants: frozen context, owner exclusions, stable rotation, distinct
draft/committed/quiet/failed/deferred states, retries yield to untried ones,
cursor progress follows attempted participants (a small budget does not favor
leading entries). Usage counts settled attempts and outstanding reservations; restart and cancellation never refund unknown use.
Drafts keep a stable mutation key, so receipt recovery needs no second model
call. It authorizes nothing; the native recipe owner wires context,
operations, dispatcher, resources, checkpoints and receipts.
`context_mode: progressive` seals fresh context before each first attempt
(kept across retries) for sequential speakers, and an unsettled predecessor
cannot become evidence; the default snapshot mode is concurrent.

## Experience classes

`installation_review.rs` revalidates the admitted experience classes
(`overlay-draw`, `narration`) by recomputing schema, ceiling and
implementation-plan digests, only for a dispatch-Ready descriptor (both
admitted descriptors are `Conditional`, so both fail closed).
`threat_model.rs` carries six red cases, including R2
`app-voice-invocation-phrases-stay-unadmitted`. Admission:
`magician_v2::apps::experience_capability`
([app-interactive-capabilities.md](../magician/app-interactive-capabilities.md)).

## Custom-surface review and scripted host

`custom_surface_review.rs` owns owner review for `custom_surfaces_v1`:
entry points and executable-member digests, a static asset scan (an aid,
never a boundary), pinned `allow-scripts` sandbox and deny-egress CSP, and
per-entry-point narrowing (empty grants no surfaces). `installation_review.rs`
folds it into the review-material digest, approve receipt and grant revision:
`AppGrantRevision.granted_custom_surface_entry_points` persists the attested
`(route, document, digest)` set, joins the authority digest when non-empty, and
diffs as `custom_surface_entry_points` in `compute_permission_diff`.

`surface_scripted_host.rs` compiles the host plan and serves only
digest-verified `surfaces/` bytes from the exact live revision.
`compile_scripted_surface_host_plan` admits only a granted `(route, document)`
whose live document matches its digest (`EntryPointNotGranted` /
`GrantedDigestStale`). The `entry_url` carries the session reference as its
first path segment (a sandboxed frame's opaque origin sends no headers);
`serve_asset` serves verified siblings with `no-store` (`DigestMismatch`
otherwise; a drifted entry document refuses its siblings too; script-capable
non-entry documents and wasm refused). `AppScriptedSurfaceRequestScope`
(principal + workspace) from `open_host` is lent by `session_scope` only while
the session lives, never serialized, so a credential-less frame GET
(Cloudflare Access identity, no `X-Workspace`) avoids `app_workspace_required`;
the bridge POST keeps the full envelope contract. Bridge methods are the eight supported-public
operations (`AppSurfaceV1Method` → `AppPublicOperationId`). Budgets: 15-minute
TTL, 32 messages and 256 KiB per session, 8 sessions per installation,
3-strike reload/crash quarantine; TTL rechecked in `serve_asset`,
`session_scope` and `note_reload` (which also checks the installation binding);
expired entries are evicted on touch, and `open_host` sweeps them before
counting the per-installation budget. Teardown maps to
`AppCustomSurfaceTeardown` (frame replaced by a "failed safely" notice). Red
rows T1–T12 live in `threat_model.rs` (T1–T4, T6, T8 kernel tests in
`surface_scripted_host.rs`; T5 in
`magios/MagiosTests/AppSurfaceScriptedHostTests.swift`; T7 owner-ratified
residual; T9–T11 `DormantBoundarySpecified`). Reference consumer
`magician_data_v3/system/thinking_map/app`; full contract:
[custom-surfaces-v1.md](../magician/custom-surfaces-v1.md).

`app_directory.rs` entries carry `custom_surface_entry_count`
(`AppPackageDirectoryMetadata` at admission, always sent, capped at eight); iOS gates "Open
app" on > 0; unified-ui treats absence as 0 and also checks `default_route`.

## Review: memory, secrets, hosts

New review/diff axes join digests only when present, so existing installs keep
their digests (`update.rs` `compute_permission_diff`).

- **Memory read** (`app_memory_read_v1`): `requested_memory_read`;
  `AppInstallationApproveRequest::granted_memory_read` must echo the digest and
  stay within it (omitted = default: non-sensitive tiers and agents while in
  use, nothing in background); a choice for an app that requested nothing is
  refused. `AppPermissionDiff::memory_read` flags new tiers or agents;
  purge removes `app_memory_read_grant_heads`
  ([app-memory-access](../magician/app-memory-access.md)).
- **Secret use.** `requested_secret_uses` (tool, secret, required,
  destination; `destinations`, `app_granted_hosts`, `delivery: config_file`)
  from each locked OS-jail tool (`secret_access::app_secret_use_requests`).
  `granted_secret_uses` is a unique subset (omitted grants nothing);
  `AppPermissionDiff.secret_uses` marks new pairs, added hosts or "any site"
  `expanded`, fewer hosts `narrowed`.
- **Key scopes.** A host-less tool's key takes `AppSecretUseGrant::hosts` (the
  app's named hosts) or `any_site` (only when the app asks for any public
  host); a `*` tool only `any_site`; a host-declaring tool neither
  (`validate_secret_use_grant`, `not_grantable`). Unscoped legacy grants
  validate (`validate_stored_secret_use_grant`) but are not granted at call
  time and drop on re-approval (`without_unscoped_legacy_grants`).
- **Tool runtime.** `tool_runtime` holds one `AppReviewedToolRuntime` per in-place or
  host-declaring OS-jail tool (`in_place_from`, `declared_hosts`,
  `reaches_granted_hosts`, `reachable_hosts`: declared hosts the app names, or
  every app-named host when it declares none), built by
  `os_jail::reviewed_os_jail_runtime`.
- **Any public host.** Offered (`offers_any_public_host`) only for
  `external_egress: any_public_host` with a host-less in-place tool;
  `granted_any_public_host` defaults false (refused unless offered), sits on
  `AppGrantRevision`, joins the authority digest only when set, and diffs as
  `expanded`.
- Revalidation passes each skill directory to
  `revalidate_locked_os_jail_artifact`; an in-place tool that cannot run shows
  the exact reason, and a changed skill needs re-approval. See
  [app-os-jail-egress](../magician/app-os-jail-egress.md#in-place-skills-inplaceskill-app_in_place_skill_v1),
  [secrets](../magician/app-os-jail-egress.md#secrets-api-key-skills-app_secret_use_v1),
  [granting any host](../magician/app-os-jail-egress.md#granting-it-app-owner).

## Older-data maintenance

`AppEntityRetentionService` owns age cleanup for installations with indexed
timestamps (preview, confirmation, durable batched progress, reference protection via
the registry and erasure owners; no package update or LLM call). Host-owner
only, absent from the sandbox bridge and workflow grants; purge removes its
job/candidate metadata ([entity-store contract](../magician/app-entity-store.md#owner-approved-age-cleanup)).

## System seed packages

System-class seeds live at `magician_data_v3/system/*/app/` (`claims_review`,
`thinking_map`, `meetings`, `town_square`, learning), matched by the docs-guard
glob.

- **Generated artifacts.** `.magician/app-derived.json`, `sdk/app.generated.ts`
  (`magician app check --write-generated`); missing or stale ones are refused,
  the bundle digest covers them; regenerate after any manifest change.
- **Versions.** Package versions seal dependency locks; a changed primitive
  binding needs a bump and regenerate of every affected seed, even with
  unchanged workflows (reusing a version keeps the old lock by design). **Any byte
  change needs a version bump**, even a `SKILL.md` link (boot refuses "changed bytes without a version
  bump"): bump `version:`, regenerate, and update
  `shipped_system_packages_carry_the_current_primitive_relock_versions`. New
  revisions go through update review and never silently replace grants.
- **Boot enablement**:
  `AppInstallationReviewService::review_and_approve_with_catalog` (one review
  preparation per package, consumed by approval; all checks still run;
  caller-supplied scope catalog; trusted system-package host grantor only).
  `scope_primitive_catalog` resolves the catalog on the blocking pool, as
  ordinary review preparation does. Owner routes keep `review()` / `approve()`.
- **Boot replay** recognizes an enabled or disabled system package at the
  shipped revision through its retained installation and committed attempt
  (in-place updates keep the installation ID); `UpdatePending` with no ready attempt is repaired by the fenced
  update publisher (ID, records and grant kept; pending until reviewed).
- **Budgets.** Foreground runs reserve ≤ 131,072 tokens and $0.50 (Town
  Square 2,097,152 tokens and $4) and 600 active seconds; app workflow calls
  use the `op-app-workflow-local` / `-remote` profiles, whose 32,768-token
  window is reserved per call. Monthly: 400,000 (Brainstorm, Learning),
  600,000 (Claims), 900,000 (Meetings), 134,217,728 (Town Square). Town
  Square's ambient behavior: 2,097,152 tokens, $4 and 600 s per run (65,536
  tokens and $0.25 per participant), at most 12 starts per hour, and
  134,217,728 tokens / $500 per month (`magician_data_v3/system/*/app/SKILL.md`
  `resources`).
- **Meetings**: six `meetings_data` read-binder actions plus five capture
  controls (not tool actions) written as one immutable `control_request` row
  each and applied only by `magician.meeting-control` through the entries
  `/meetings` uses; `surfaces/console.js` keeps a STOP reserve in its
  32-message budget, stretches its interval as the pool drains, and shows the
  remaining count ([meetings-surface.md](../magician/meetings-surface.md)).
- **Town Square**: `behaviors`, `llm_operations`, `notification_ports`; its
  store is the corpus **authority**, so stop posting with
  `policy.autonomy_state`, not by disabling (no host substrate is kept).
  Console: the write reserve is metered against the poll budget, and the policy
  form re-seeds from a poll only while untouched. Setup is detected from
  `turn_cursor`, not `policy` (when missing the console launches `sync_roster`
  once). Scheduled gate and compose run before corpus reads with no persona or
  recent feed, so a completed empty-draft turn may only advance the cursor and
  is not proof of a post
  ([town-square-app.md](../magician/town-square-app.md)).
- **Memory Learning** `sync_queue`: deterministic recipe, forwards only `limit`
  and `state` to `internal_data.list_learning_candidates`, preserves nullable
  fields, IDs and records outside the page, and completes through the terminal
  transaction without model generation.
- **Claims Review** mirrors `/claims-review` with a read-only register sync
  that atomically projects a bounded source page and its continuation cursor,
  keeping full statement wording
  ([Claims Review](../unified-ui/claims-review.md#installed-app)).

## Tests

- `make test-app-reconciliation`: reconciliation, effect admission, registry
  contention (controlled clock).
- `make test-app-registry-admission`: admission and profiling on real
  SQLCipher, without recompiling the monolith.
- `make test-app-indexed-query`: production SQL planner (10,000 records).
- `make test-app-contextual-round`: progression, context refresh, recovery,
  shipped recipe.
- A schema-35 fixture regression upgrades to the current registry schema (38)
  keeping records (`age_cleanup_migrates_v35_in_a_fresh_process_without_losing_records`).
