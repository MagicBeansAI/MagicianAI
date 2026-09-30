# Storage Abstraction

Magician's compute placement and durable-storage placement are independent
configuration choices. This page is the living contract for that split. It
does not change the default local profile. The completed program plan is
archived at
Complete Storage Abstraction and Typed Storage Capabilities.

## Source of truth

[`storage-catalog.yaml`](storage-catalog.yaml) is authoritative for:

- owner identity
- class (`authoritative`, `lifecycle_managed`, `regenerable`, `observability`,
  `restricted`, `secret`, `ephemeral`, `bootstrap`, `external`, `device_local`)
- tier (1 = non-regenerable truth, 2 = regenerable projection, `device_local`)
- readiness / authority / legacy-source state
- writers, readers, and lifecycle owner

The `/storage` operator snapshot is a **projection**. Live construction lives
in `magician-comms/src/channel_assist/governance.rs`; a test-fixtures copy
lives in `magician/src/magician_v2/storage_governance/mod.rs`. Every
`StorageEntry.id` must appear as exactly one catalog `governance_id`, and every
catalog `governance_id` must appear in both Rust copies.

`scripts/storage_catalog_guard.py` (`make check-storage-catalog`, part of
`make check-all`) fails on schema/enum violations, a readiness state without
its evidence (or any predecessor's), drift between the two Rust inventories or
between them and the catalog, an on-disk `Connection::open` /
`open_with_flags` file not listed in `measurements.direct_io_files`, and a
listed owner/writer/reader path that does not exist.

Do not maintain a second hand-written inventory. Extend the YAML, then keep
the Rust projection in lockstep. "Reconcile later" is not an allowed state.

The restricted HMAC catalog owner includes `restricted/task_recipe_replays/`:
authoritative exact-execution API replay receipts, not regenerable recipe
statistics. The Artifact workspace provider atomically publishes and
bounded-reads them; scope HMACs bind execution identity and saved state. They
survive recipe deletion/mining disablement so restart can recover answers or
stop uncertain mutations without redispatch. Settlement projects Artifact
before making Runtime terminal, so partial commits stay discoverable through
the active-execution recovery scan.

`WorkspaceFileProvider` (`magician/src/magician_v2/artifact_v2/workspace.rs`)
is the local compatibility adapter (`local_file` and `silverbullet_space`) for
product stores. It is not a remote-storage boundary.

## Process runtime

`magician-storage` owns `ScopeId` / `StorageKey`, `StorageError` retry
classes, capability traits, bootstrap profile parsing, local adapters, and
`StorageRuntime`. Domain repositories stay in owner crates. Default startup
without a bootstrap file uses the local `workspace_storage` provider.

`magician_storage::LocalStorage::open` wraps a directory tree behind the typed
capabilities. Local `ObjectStore` uses per-object sidecar metadata
(`.objmeta.json`) plus a per-key lock for create-only, expected-version
put/delete, legacy import, and crash recovery. Sidecars are adapter-owned and
are not locators.

`magician-bin` constructs one `StorageRuntime` from the bootstrap profile and
the resolved workspace (adapters under `.magician-storage/`), acquires the
default scope lease on the server path, calls `StorageRuntime::install` after
`open_local`, and exposes `GET /health/storage`. Classified live writes go
through cataloged owner kits (`typed_io`); `ArtifactV2Workspace` remains the
adapter for unclassified, scratch, notes, and JSONL commit-marker paths.
`open_local` refuses remote profiles. A second process targeting the same local
scope is refused; distinct scopes stay independent.

Satellite crates use `magician_v2::process_storage` (`current` / `require` /
`runtime_root` / `workspace`) instead of resolving `MAGICIAN_ROOT_DIR`. Only the
composition-root bootstrap (`magician-bin`, config load) may call
`default_storage_base_path` before install.

Remote backends (all dormant; `magician-bin` does not depend on them):

- **PostgreSQL** is the chosen remote transactional backend; local embedded
  stays SQLite behind the same repository traits; hosted SQLite/libSQL is not
  selected. Spike: `magician-storage-gate1` (`make test-storage-gate1`), never
  opens user data. ADR:
  [adr-2026-08-31-remote-transactional-backend.md](../magician-storage/adr-2026-08-31-remote-transactional-backend.md).
- **`magician-storage-s3`**: S3-compatible `ObjectStore` and `DatasetStore`,
  constructible only from an explicit `remote_durable` profile or an in-memory
  backend. `http://` endpoints are refused, credentials stay in `SecretStore`
  and out of `Debug`, encryption must be `required`. `make test-storage-s3`.
- **`magician-storage-state`**: SQLite/Postgres construction pattern (pool,
  schema ledger, idempotency/outbox, export/import, SQL leases). Default startup
  uses filesystem leases. `make test-storage-state`.
- **`magician-storage-migration`**: qualification framework — owner migration
  registry, coordinator APIs (inventory, plan, export, import, checkpoint,
  resume, semantic verify, rollback, status, prepare-cutover / cutover /
  settle), a durable JSON ledger, crash/restart at every phase, per-owner source
  guards, and scenario factories in `magician-storage::conformance`. Readiness is
  monotonic: `remote_ready` needs every predecessor evidence key. Every catalog
  row is `remote_ready` while authority stays `local_active` and local sources
  stay `retained`. `make test-storage-migration`. Template:
  [owner-closure-reference.md](../magician-storage/owner-closure-reference.md).

## Owner kits

Local layouts remain canonical; remote adapters are test-only; default startup
selects no remote repository and no startup scanner uploads existing files.

- **Immutable artifacts** (`magician_v2/object_owners/`): `durable_artifacts`,
  the app blob family (`app_packages`, `app_attachments`, `app_exports`,
  `app_captures`, `app_evaluations`), `task_outputs`, `execution_outputs`,
  `execution_observations`, `execution_downloads`, `execution_recordings`,
  `llm_trace_journal`, `llm_restricted_journal`, accepted `skills_scope` bytes.
  In-flight `.part` downloads and live CDP screencast are not durable.
- **System, devices, secrets** (`magician_v2/system_owners/`): `secret_vault`,
  `device_pairing`, `resource_authority`, `programs`, `capability_evolution`,
  `content_sources`, `transport_log_jsonl`, `compaction_metrics`,
  `system_templates`, `runtime_config`, `notes_provider`,
  `silverbullet_space_runtime`, `device_local_imessage`,
  `device_local_whatsapp`. Secret export is metadata-only
  (`secret_audit.jsonl`). Device-local message databases and the notes provider
  are classified, not migrated. Bootstrap config and system templates stay
  host-managed. Pairing roster writes keep their sealed persist path.
- **Subprocess and skill storage**: `workdirs_scratch`,
  `subprocess_skill_working`. `workdirs/` and `workdirs/skill-working/{call_id}/`
  stay ephemeral until explicit acceptance. The governed launcher writes a
  versioned `SubprocessStorageEnvelope` (scratch lease, input manifest,
  SecretRef requests, output slots, result manifest); it does not replace an
  admitted CLI working directory or a `WorkingDirectoryMode::Denied` contract.
  Closed children get no `MAGICIAN_ROOT_DIR`, `MAGICIAN_STORAGE_PATH`, or
  store credentials. Accepted slots publish through the object kit; unaccepted
  and cancelled scratch is scavenged. Reader: `subprocess_owners::load_envelope`
  (`ENVELOPE_VERSION`, `ENVELOPE_FILE_NAME`).
  `skillshub/scripts/runtime_root_shim.py` translates old runtime-root paths and
  emits a deprecation metric; `scripts/skillshub_runtime_root_linter.py` runs
  in the same gate as `scripts/docs_guard.py`.
- **Desktop engine-path reads**: `desktop_engine_roots`. Wake-word models, OS
  permission state and Application Support config are device-local or
  bootstrap. The desktop reaches engine-owned data only through
  `engine_base_url` (`None` → `http://127.0.0.1:{magician_port}`).
  `is_remote_engine()` skips local container supervision and refuses
  engine-owned filesystem reads explicitly; device-bound browser/screen/audio
  automation may be unavailable against a remote engine.
- **SQLite and DuckDB lifecycle stores** (`magician_v2/database_owners/`):
  `analytics_duckdb`, `channel_assist_duckdb`, `ui_threads_duckdb`,
  `feed_duckdb`, `browser_engine_usage_sqlite`, `app_store_sqlite`,
  `social_sqlite`, `api_mining`, `attention_learning_sqlite`,
  `attention_funnel_sqlite`, `resurfacing_sqlite`, `hitl_lifecycle_sqlite`.
  `storage_governance::duckdb_compaction` is the operator job over those files;
  the `storage_governance` inventory is a catalog projection; `scratch_tool_duckdb` is ephemeral. Path helpers:
  `database_file_path` / `host_database_path`. WAL sidecars belong to the same
  owner. A missing `feed_duckdb` must not empty `social_sqlite`. Host attention
  files are shared at the runtime root with scope-owned rows.
- **Agents, memory, learning** (`magician_v2/agent_owners/`):
  `agent_definitions`, `agent_runtime`, `memory_canonical`, `memory_index`,
  `learning_procedures`, `learning_procedure_index`, `learning_histories`.
  `AgentStorage` publishes whole files via `persist_agent_file`; `LearningStore`
  uses `persist_agent_file_sync`. JSONL appends and LanceDB stay specialized
  local writers. A missing index must not empty canonical memory or procedures.
- **Chat and progress** (`magician_v2/chat_owners/`): `chat_sessions`,
  `chat_messages`, `chat_transcripts`, `chat_turn_events`, `chat_enrollments`,
  `progress_channels`, `progress_subscriptions`, `progress_lineage`.
  `FileChatStore` publishes `session.json` through `persist_chat_file`. JSONL
  appends and transcript segment+manifest commits stay specialized local
  writers. `ChatStore` is not the remote boundary. A missing `progress_lineage`
  must not empty `progress_channels`.
- **Work spine** (`magician_v2/work_owners/`): `task_records`,
  `internal_tasks`, `task_plans`, `executions`, `task_planning_recovery`,
  `pause_states`, `list_index`. Monitors are ordinary scheduled tasks on
  `task_records`. `FileV2Store` publishes `execution.json` through
  `persist_work_file`. The task multi-write journal, pause envelope rewrite,
  `list_index` SQLite and execution event JSONL + `events.jsonl.commit` stay
  specialized local writers. A missing `list_index` must not empty
  `task_records`.
- **Parquet datasets**: `events`, `memory_events`, `activity_rows`,
  `activity_rollups`, `llm_calls`, `llm_embeddings`, `llm_provider_attempts`,
  `llm_tool_calls`, `llm_capture_gaps`, `llm_dispatch`, `llm_call_io`,
  `llm_context_blocks`, `llm_content_tombstones`, `llm_content_access_audit`.
  Local `dt=*` (activity `dt=*/hour=*`) layouts are canonical; DuckDB readers use
  `family_read_glob` / `family_parquet_glob`. Compaction, checksum and retention
  stay in `parquet_maintenance`; `analytics_duckdb` is a regenerable query
  catalog over them.

## Live workspace I/O

`ArtifactV2Workspace::write_path` / `write_atomic_path` / `read_path` classify
the locator and persist through the owner kits. Unclassified paths (scratch,
ephemeral, notes) stay on the file provider. DuckDB `COPY TO` parquet is
published with `publish_written_parquet` after the durable rename; outside
`scopes/` or a dataset family, the rename already is the publish. Compaction
locators include `dt=*/_compact/*.parquet`.

Canonical `events.jsonl` plus its independent `events.jsonl.commit` byte-length
marker stay on `WorkspaceFileProvider` so whole-file persist cannot skip the
fsync/rename uncertainty protocol. Live SQLite/DuckDB engines `Connection::open`
on `database_file_path` / `host_database_path` — the local `domain_repository`
engine, not an `ObjectStore` blob. Device-local iMessage/WhatsApp ingest stays
outside the kits. `StorageRuntime` object/dataset adapters stay unselected until
Track B cutover.

SaaS-split seams: `api_scope::ResolvedScope` extracts `magician_storage::ScopeId`
for durable-artifact REST and realtime WS. `DeviceTransport::LocalLoopback` is
installed at boot; browser CDP reads it.

## Typed capabilities

The catalog names the intended capability per owner:

| Capability | Holds |
| --- | --- |
| `domain_repository` | Transactional product truth |
| `object_store` | Immutable/versioned blobs |
| `dataset_store` | Parquet generations + manifests |
| `index_store` | Derived search with a source watermark |
| `lease_store` | Fencing |
| `scratch_store` | Local materialization whose loss is expected |
| `secret_store` | Non-serializable credentials |
| `notes_provider` | Human-editable notes; never canonical runtime by implication |
| `device_local` | Paired-device state, not server tenant storage |
| `bootstrap` | Host-managed config outside the selected stores |

The Settings Runtime provider control stays; switching it does not move data.
`local_file` and `silverbullet_space` are shipped local variants of the same
layout; `silverbullet_space_runtime` is a catalog row so that variant cannot be
deleted as a "ratchet".

## Recovery, activation, and profiles

Cross-owner recovery lives in `magician/src/magician_v2/recovery/`. Local owner
snapshots use each kit's `export_all`. Restore onto a fresh host re-imports the
bundle (objects, then datasets, then repositories), walks object
version/digest and dataset parts, and rebuilds indexes on an empty
scratch/index root. Archives are integrity-signed; vault bytes stay out of
backups. Object tombstones retain bytes for seven days; GC cannot collect a
referenced version or the live replacement generation. Dataset GC drops
generations absent from the current manifest. Credential rotation does not
rewrite product bytes. Default startup does not snapshot or collect garbage.
Objectives:
[adr-2026-09-01-recovery-objectives.md](../magician-storage/adr-2026-09-01-recovery-objectives.md).

The default profile is `local_embedded` with no required bootstrap file. Every
Tier 2 owner has a rebuild or unavailable contract in
[capability-support-matrix.yaml](../magician-storage/capability-support-matrix.yaml).
Deleting scratch and rebuildable indexes does not drop Tier 1 bytes. An offline
cutover freeze of 900 s is accepted, so online migration is optional
([adr-2026-09-01-performance-capacity.md](../magician-storage/adr-2026-09-01-performance-capacity.md)).

Operator workflow: `magician storage inventory|plan|export|import|verify|cutover|rollback|status`,
`GET /api/magician/v2/storage/activation`, and
`POST /api/magician/v2/storage/activation/cutover`. Cutover requires every
precondition (Decision Gate 3, a recent backup, remote health, no competing
lease, rollback retention) and the confirmation `CUT OVER STORAGE`. Settings and
the command palette expose inventory/status only. See
[task-19-activation.md](../magician-storage/task-19-activation.md).

Profile acceptance kit: `magician/src/magician_v2/profile_acceptance/`.
Browser/screen/meeting/audio are `device-bridge-required`; the four
device-local owners are `unavailable` under `remote_durable`.

## What a scope is for

A scope is created implicitly: any write runs `create_dir_all` on its parent, so
a subsystem that merely *looks up* a scope materializes it. `ScopeProfile`
(`artifact_v2/workspace.rs`) makes the distinction the storage layer could not
otherwise express.

| profile | scopes | meaning |
| --- | --- | --- |
| `User` | everything else, the default scope included | a real tenant; every subsystem may materialize |
| `ReservedSink` | `system`/`system`, `_quarantine`/`_quarantine` | catches records belonging to no tenant — no owner, no surface, no inbox |

Three enumerations carry the rule, and a test asserts they agree:

| enumeration | tenant-only counterpart |
| --- | --- |
| `list_scopes()` | `list_tenant_scopes()` |
| `list_scope_segments_sync()` | `list_tenant_scope_segments_sync()` |
| `list_scope_segments()` | `list_tenant_scope_segments()` |

`scope_hosts_user_subsystems()` is the predicate. It gates only tenant
subsystems (apps, mail, UI feed and threads, social, api-mining); event and
analytics state is never gated, since holding those is what a sink is for.
Enumeration is unaffected — the defect is materializing, not listing. Eager
sweeps (artifact verification, app boot admission and slot defaults,
content-source observation, agent-memory resolution, recipe verification,
vibedev runs, taste capture, feed and ui-threads materializers, attention
historical bootstrap, actionability training, and the two `background_scopes`
helpers in `magician-comms`) enumerate tenants only. Analytics and the
transport-log event stream keep `list_scopes()`.

A sink still gets first-touch residue (lock files and empty catalogs created by
a path being touched, not by a sweep); gating those would mean guarding
per-request code where a wrong gate is a runtime failure, so it is left. To find
an unexpected opener in a sink, measure which file handles the process holds
(or log the scope on first open, e.g. in `registry_connections()`), rather than
reading enumeration call sites.

The predicate is deliberately narrow. `anonymous`/`default` is a real tenant and
the scope a single-user deployment uses; widening to "anything system-ish" would
ship a deployment with no apps. `system`/`notes` and `anonymous`/`system` are
tenants too; tests pin all of these.

## Typed storage boundaries

`scripts/check_typed_storage_boundaries.py` (`make check-typed-storage-boundaries`)
rejects new implicit-path storage. Allowlist only reviewed adapters, owner kits,
backup/export, the `magician-bin` composition root, and remaining
local-compatibility `default_storage_base_path` sites. Reaching `remote_ready`
does not delete retained local sources. Copying a scope directory is not a
migration. See
[task-21-typed-storage-boundaries.md](../magician-storage/task-21-typed-storage-boundaries.md).

Track A prepares owners to `remote_ready` while local stays canonical (no
default-profile change, no required config, no remote I/O). Track B is an
explicit operator cutover after Gates 1–3.

## Guard and docs

- Catalog: [`storage-catalog.yaml`](storage-catalog.yaml)
- Operator surface: [Storage Governance](storage-governance.md)
- Workspace paths: [Workspace File Provider](workspace-file-provider.md)
- Guard: `scripts/storage_catalog_guard.py`
- Tests: `scripts/test_storage_catalog_guard.py`
- Task 21 ratchet: `scripts/check_typed_storage_boundaries.py`
  (`make check-typed-storage-boundaries`)

## Published process chat store

Boot publishes the initialized process chat store once
(`chat::storage::publish_global_chat_store`, from `magician-bin` after the index
is built) so read owners constructed without server wiring (compiled read
providers rebuilt per scope) reach the same index instead of re-scanning the
chat root.

- **Read-only by contract.** Every mutation goes through the owner that holds
  the store.
- **First publication wins**; a late or duplicate publisher is logged.
- **Absent is fail-closed.** Without a publication `global_chat_store()` returns
  `None` and readers refuse rather than building their own.

Consumer example: the `meetings_data` binder uses the index-only
`ChatStore::list_thread_summaries_for_prefix` so a polling surface does not
re-read the whole meeting history per tick ([meetings-surface.md](meetings-surface.md)).

## Town Square corpus authority

The `town-square` package's app entity store is the **authority** for the Town
Square corpus; the first-party SQLite store (`social.db`) is only a migration
source. See [town-square-app.md](town-square-app.md).

- `/social/*` reads and writes the app entity store. `social.db` is still opened
  at boot, governed and pruned, but served to no user.
- Disable is not substrate-preserving for this corpus: there is no host store
  underneath.
- The migration (`town-square-migrate`) is the only bridge; its per-table
  count-and-digest proof is load-bearing. History-only migration writes through
  the scoped Apps entity-store owner, verifies with paginated read-back proofs,
  retains the source corpus, and does not rewrite live membership or control
  records.
- `spend_log` and `social_budget` are no longer created, counted or retained,
  with no `DROP TABLE` (retirement is a code change, not a data deletion); code
  must not read them, since on a new store the `SELECT` fails.
  `prune_history` keeps and ignores `max_spend_rows`.
- `SocialStoreRegistry` has exactly two legitimate readers: storage governance
  (retention/lifecycle) and `town-square-migrate`. A second reader of a store
  that no longer receives writes reports stale rows as healthy. The registry
  must outlive the migration on every deployment.

## Definition-store handle sharing

`AgentDefinitionStore` carries an `Arc<DefinitionCache>` invalidated by writes
through that same handle, so two independently constructed stores over one root
are two caches that never see each other's writes. Boot hands the runtime's
shared `Arc<AgentDefinitionStore>` to the `agent_roster_data` and `memory_data`
binders (`tasks_data` and `notes_data` hold no definition store).

- **Freshness rides on the handle.** An out-of-band YAML edit is invisible to
  every handle.
- **Scope is verified, not assumed.** `for_scope` is a no-op on a store built
  without a workspace layout; a scope-owned reader must confirm the re-point
  landed, because the failure silently reads the wrong scope.
- Read-only callers that project a few fields should use
  `list_definitions_shared`: `list_definitions` deep-copies every record because
  the cache holds the `Arc`.

## System-package boot admission

- Boot admits the deployment's `distribution: system` packages from the seed root
  before any worker or route can look for them, and before the app projection
  worker is spawned (a mid-admission "not installed" answer would be cached
  reasoning about state about to change). The admitted set is
  `meetings, town_square, claims_review, learning, thinking_map`; staging and
  publication refuse a system manifest without that owner, so a seeded package
  missing from the list `run_http_server` names is silently unreachable.
- `ArtifactV2Workspace::system_seed_root()` is canonicalized before use because
  `admit_package_directory` refuses `.`/`..` components (a relative or symlinked
  data dir would fail every package). Package directories beneath it are not
  canonicalized; they go through the `O_NOFOLLOW` walker.

## Task directory husks

`acquire_task_write_guard` creates the task directory and its
`.artifact_v2.lock` before anyone knows a write will follow, so a failing op
under `with_task_write_lock` would leave a directory holding only the lock.
`with_task_write_lock` discards such a directory when its op errors — safe only
there, because the exclusive guard is still held and a lock-only directory has
no data. Best-effort and silent. Moving lock files out of the task directory was
rejected: during a rollout old and new binaries would lock different paths and
not exclude each other.

The planning-catalog bootstrap skips a task directory with no state file (a
husk) at debug level and still counts the scan complete — the completion marker
is what stops the scan re-running every boot. A state file that exists but
cannot be read still defers the marker.

## Runtime env files

`magician-bin` loads `$MAGICIAN_ROOT_DIR/.env.development` and `.env` in `main`,
before config load and any credential read, resolving them through
`workspace::runtime_config_path`. Nothing upstream does it: `magic-supervisor`
forwards only its inherited environment and `scripts/run-supervisor.sh` sources
nothing, so the process that knows the runtime root must load them.

## Evidence completion journal

Evidence decisions append to a durable, densely-numbered completion journal,
segmented at 512 entries per file so a page opens one file. The register row
lands **before** the journal entry (journal-first would announce completions
that never happened); every decision path also journals on its replay branch,
idempotent by decision id, which repairs a crash between the two. Cost: a
failed journal append reports an error although the row landed; the retry
returns `already_applied` and repairs the journal.

## HITL lifecycle

- **Resolution is terminal for a correlation id.** The reducer flags a request
  against a resolved record *invalid* rather than reopening it; a journal ending
  in a request replays to `Resolved`.
- **The durable authority outranks in-memory recovery health.** The lifecycle
  journal commits before registry, canonical and broadcast visibility.
  `hitl_lifecycle_recovery_healthy` guards only the no-authority fallback and
  authority creation; it does not suppress a durable answer.

### Generation-bound app resolution stays side-effect free

Once the V3 lifecycle authority is ready, the generic
`emit_hitl_lifecycle_if_accepted` receipt refuses app-owner request and
resolution events before acquiring publication authority; only the
generation-bound reconciliation APIs advance those records. A tokenless
resolution must not wait out a live ticket's cross-process lock and then use
append-failure settlement to mark the record `Resolved`; the refused call leaves
the pending proof and unconsumed ticket for the exact durable UserRequest owner.

App-run list classification uses the reserved canonical task identity. New runs
use the internal task root; legacy app runs stay in their original directory and
are projected as internal. Index rebuild and periodic reconciliation include
those legacy rows in the internal unit so orphan sweeps cannot erase them.

## Envoy Claims Review

Observed transcript ingestion serializes retries by transcript key. A retry must
match the recorded act's sender, recipients, channel, work references and
consequence class before adding candidates, including after a partial import.
Candidates also bind relationship, occurrence time, extractor and source
segment; a changed import requires a new key. Replay lookup binds the source
segment even if the catalogue claim reference changes, so a new derived ID
cannot requeue a rejected statement or duplicate a review decision.

New imports append an `ingestion_prepared` fingerprint to the scoped claim log
before writing the observed act or any candidate, binding the complete source
and utterance metadata (retry time excluded). All candidates are validated
before any are published; duplicate candidate references refuse the batch.
Historical imports without a fingerprint are checked against every existing act
and candidate; their first valid resumption establishes the binding. Identical
retries preserve earlier review decisions. An owner can recover an unfinished
confirmation's saved command after losing browser state; the read takes the
claim lock, checks revision and request fingerprint, and writes nothing.

Envoy reply capture reuses the scoped outward-assertion and transcript-claim
stores. Dispatch serializes on per-message locks under the workspace scope
(`envoy_delivery_locks`); an uncertain attempt never authorizes a resend. Each
lock has an atomic `<message-key>.json` delivery binding (attempt UUID, address,
payload digest — no reply text), written through the workspace storage owner
before the outward act enters `dispatching`. Missing claim preparation or a
different binding never grants dispatch. See
[Claims Review](../unified-ui/claims-review.md).

## Testing notes

### Workspace root in test harnesses

`MagicianV2Orchestrator` resolves `durable_artifact_workspace` from
`services.workspace_layout`, else `resolve_scoped_root(runtime_config.storage_path())`.
That fallback lands on the **seed** root (`config.storage_path` defaults to
`"magician_data_v3"` and `resolve_scoped_root` is the identity), not the runtime
root (`resolve_storage_workspace`: env override, else `$HOME/MagicianNotes`).
Production passes `.with_workspace_layout(storage_workspace)` in
`magician_core/builder.rs`; test orchestrators
(`build_test_orchestrator_inner_at`) must wire the layout the same way, or the
service and orchestrator disagree about where scope state lives.

Under the harness the runtime root can still be the operator's
(`MAGICIAN_ROOT_DIR` in the shell wins), so crate code that sweeps or seeds a
tree at first use must not decide on `cfg!(test)` (false in the library an
integration test links); use `workspace::running_under_cargo_test_harness()`.

## Activating a test scope for the stateless driver (2026-09-04)

The stateless driver — the default, and the only one selected implicitly —
refuses to run in a scope whose legacy-writer cutover is not sealed. Operators
seal once per scope with `magician seal-stateless-loop-cutover`; fixtures must
too, because that is a deployment precondition, not behaviour under test.
`test_support::activate_conventional_test_scopes_sync(runtime_root)` seals the
conventional scope names; the shared artifact-v2 harness and
`build_orchestrator_with_ask_loop` call it, and a fixture with its own storage
root should too.

**Not the alternative:** `MAGICIAN_EXECUTION_DRIVER=inprocess`. `Inprocess` is
never selected implicitly, has no durable outbox drain, and has different
restart guarantees; it turns failures green by not exercising the production
driver.

### Fixture rules

- **Canonicalize symlinked temp roots; never relax `SQLITE_OPEN_NOFOLLOW`.**
  macOS `temp_dir()` is under `/var`, a symlink to `/private/var`, so the HITL
  lifecycle authority correctly refuses it (surfacing as `unable to open
  database file`). Use `magician-api`'s `canonical_tempdir` or
  `realtime_events`' `canonical_workspace_tempdir`.
- **Registry migration fixtures must be real.** Build an old `user_version`
  database by applying the real schema prefix (`APP_REGISTRY_SCHEMA` …
  `APP_REGISTRY_SCHEMA_V19`), not a hand-rolled subset — later migrations touch
  older objects. Do not relax a migration (`IF EXISTS`) to fit a fixture.
  Include the `app_registry_scope` row, or the open fails with `ScopeCollision`.

### Rust pitfalls in the app-platform SQLite paths

- **`MappedRows` in a block tail.** `query_map`/`query` return iterators that
  borrow the statement; `stmt.query_map(..)?.collect::<Result<Vec<_>,_>>()?` as a
  block tail is E0597 because the `?` temporary outlives `statement`. Bind the
  iterator to a local first, then collect. `query_row`/`execute` are unaffected.
- **`execute_scoped_typed_*` error inference.** These are generic over
  `<T, E, F>` with only `E: From<AppRegistryError>`, so a closure whose `?`
  spans several error types hits E0283. Annotate the closure's return type
  (`move |connection, _| -> Result<T, MyError> { … }`) rather than turbofishing.
  `F: 'static`, so closures reading `now` must `move`.
