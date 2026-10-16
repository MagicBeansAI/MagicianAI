# Storage Abstraction

Compute and durable-storage placement are independent configuration choices;
this is the contract for that split (default local profile unchanged). Plan:
Complete Storage Abstraction and Typed Storage Capabilities.

## Source of truth

[`storage-catalog.yaml`](storage-catalog.yaml) is authoritative for owner
identity; class (`authoritative`, `lifecycle_managed`, `regenerable`,
`observability`, `restricted`, `secret`, `ephemeral`, `bootstrap`, `external`,
`device_local`); tier (1 = non-regenerable truth, 2 = regenerable projection,
`device_local`); readiness / authority / legacy-source state; writers, readers
and lifecycle owner.

The `/storage` operator snapshot is a **projection**, built in
`magician-comms/src/channel_assist/governance.rs` (test-fixtures copy:
`magician/src/magician_v2/storage_governance/mod.rs`). Each `StorageEntry.id` is
exactly one catalog `governance_id`, and each `governance_id` is in both Rust
copies. Extend the YAML and keep both Rust copies in lockstep; never keep a
second inventory ("reconcile later" is not an allowed state).
`scripts/storage_catalog_guard.py` (`make check-storage-catalog`, in
`make check-all`) fails on schema/enum violations, readiness without its (or a
predecessor's) evidence, drift between Rust copies and catalog, an on-disk
`Connection::open` / `open_with_flags` file absent from
`measurements.direct_io_files`, or a listed owner/writer/reader path that does
not exist.

- **Idle owners.** One host collector retires unused scoped DuckDB owners and
  SQLCipher idle connections; requests pin owners during native work and
  reopening keeps data ([idle workspace resources](duckdb-analytics.md#idle-workspace-resources)).
  StorageRuntime authority and scope fencing leases follow the normal
  handoff/shutdown contract. Channel-assist and feed connections open through
  `storage_governance::database_maintenance`, cataloged as a shared direct-I/O
  lifecycle boundary; both stores keep their canonical owners.
- **Replay receipts.** `scopes/<p>/<w>/restricted/task_recipe_replays/<blake3>.json`
  (`artifact_v2/recipe_replay_receipt.rs`; restricted HMAC owner)
  holds authoritative exact-execution API replay receipts (not regenerable recipe
  statistics), published atomically and bounded-read by the Artifact workspace
  provider and HMAC-bound to execution identity and saved
  state. They survive recipe deletion and disabled mining, so restart recovers
  answers or stops uncertain mutations without redispatch; Settlement projects
  Artifact before Runtime goes terminal, so the recovery scan sees partial commits.
- `WorkspaceFileProvider` (`magician/src/magician_v2/artifact_v2/workspace.rs`)
  is the local compatibility adapter (`local_file`, `silverbullet_space`), not a
  remote-storage boundary.

## Process runtime

`magician-storage` ([crate doc](../magician-storage/README.md), including
`magician_storage::LocalStorage::open` and its `.objmeta.json` sidecars) owns
`ScopeId` / `StorageKey`, `StorageError` retry classes, capability traits,
bootstrap profile parsing, local adapters and `StorageRuntime`; domain
repositories stay in owner crates. No bootstrap file → local `workspace_storage`.

`magician-bin` builds one `StorageRuntime` (adapters under `.magician-storage/`),
takes the default scope lease on the server path, calls
`StorageRuntime::install` after `open_local` (which refuses remote profiles) and
serves `GET /health/storage`. A second process on the same local scope is
refused; distinct scopes stay independent. Classified writes go through owner kits (`typed_io`);
`ArtifactV2Workspace` covers unclassified, scratch, notes and JSONL
commit-marker paths. Satellite crates use `magician_v2::process_storage`
(`current` / `require` / `runtime_root` / `workspace`), not
`MAGICIAN_ROOT_DIR`; only the composition-root bootstrap (`magician-bin`, config
load) may call `default_storage_base_path` before install.

Remote backends are dormant (`magician-bin` does not depend on them):
PostgreSQL is the chosen remote transactional backend, local stays SQLite,
hosted SQLite/libSQL is not selected (spike `magician-storage-gate1`,
`make test-storage-gate1`, never opens user data;
[ADR](../magician-storage/adr-2026-08-31-remote-transactional-backend.md)).
`magician-storage-s3` (`ObjectStore` / `DatasetStore`, only from an explicit
`remote_durable` profile or in-memory; refuses `http://`, credentials in
`SecretStore` and out of `Debug`, encryption `required`; `make test-storage-s3`).
`magician-storage-state` (SQLite/Postgres pool, schema ledger,
idempotency/outbox, export/import, SQL leases; default startup uses filesystem
leases; `make test-storage-state`). `magician-storage-migration` (owner migration
registry; coordinator inventory, plan, export, import, checkpoint, resume,
semantic verify, rollback, status, prepare-cutover / cutover / settle; durable
JSON ledger, crash/restart at every phase, per-owner source guards, `magician-storage::conformance` factories; `make test-storage-migration`;
[template](../magician-storage/owner-closure-reference.md)). Readiness is
monotonic: `remote_ready` needs every predecessor evidence key. Every row keeps
authority `local_active` and its local source `retained`, whatever its readiness.

## Owner kits

Local layouts stay canonical, remote adapters are test-only, default startup
selects no remote repository, and no startup scanner uploads existing files.

| Kit | Owners | Notes |
| --- | --- | --- |
| Immutable artifacts (`magician_v2/object_owners/`) | `durable_artifacts`, app blobs (`app_packages`, `app_attachments`, `app_exports`, `app_captures`, `app_evaluations`), `task_outputs`, `execution_outputs`, `execution_observations`, `execution_downloads`, `execution_recordings`, `llm_trace_journal`, `llm_restricted_journal`, accepted `skills_scope` bytes | `.part` downloads, live CDP screencast not durable. |
| System, devices, secrets (`magician_v2/system_owners/`) | `secret_vault`, `device_pairing`, `resource_authority`, `programs`, `capability_evolution`, `content_sources`, `transport_log_jsonl`, `compaction_metrics`, `system_templates`, `runtime_config`, `notes_provider`, `silverbullet_space_runtime`, `device_local_imessage`, `device_local_whatsapp` | Secret export metadata-only (`secret_audit.jsonl`). Device-local DBs, notes provider: classified, not migrated. Bootstrap config, system templates host-managed; pairing roster keeps its sealed path. |
| Subprocess and skill storage | `workdirs_scratch`, `subprocess_skill_working` | Below. |
| Desktop engine-path reads | `desktop_engine_roots` | Below. |
| SQLite and DuckDB lifecycle stores (`magician_v2/database_owners/`) | `analytics_duckdb`, `channel_assist_duckdb`, `ui_threads_duckdb`, `feed_duckdb`, `browser_engine_usage_sqlite`, `app_store_sqlite`, `social_sqlite`, `api_mining`, `attention_learning_sqlite`, `attention_funnel_sqlite`, `resurfacing_sqlite`, `hitl_lifecycle_sqlite` | Job: `storage_governance::duckdb_compaction`; `scratch_tool_duckdb` ephemeral. Paths `database_file_path` / `host_database_path`; WAL shares owner. Missing `feed_duckdb` must not empty `social_sqlite`. `feed_duckdb` failure rows are deleted after 7 days (`FeedStore::prune_failed_items`); Attention hides them after 2 h. Host attention files at runtime root, rows scope-owned. |
| Agents, memory, learning (`magician_v2/agent_owners/`) | `agent_definitions`, `agent_runtime`, `memory_canonical`, `memory_index`, `learning_procedures`, `learning_procedure_index`, `learning_histories` | `AgentStorage` → `persist_agent_file`; `LearningStore` → `persist_agent_file_sync`. JSONL, LanceDB specialized. Missing index must not empty memory or procedures. |
| Chat and progress (`magician_v2/chat_owners/`) | `chat_sessions`, `chat_messages`, `chat_transcripts`, `chat_turn_events`, `chat_enrollments`, `progress_channels`, `progress_subscriptions`, `progress_lineage` | `FileChatStore` → `session.json` via `persist_chat_file`; JSONL and transcript segment+manifest specialized. `ChatStore` is not the remote boundary. Missing `progress_lineage` must not empty `progress_channels`. |
| Work spine (`magician_v2/work_owners/`) | `task_records`, `internal_tasks`, `task_plans`, `executions`, `task_planning_recovery`, `pause_states`, `list_index` | Monitors are scheduled tasks on `task_records`. `FileV2Store` → `execution.json` via `persist_work_file`. Multi-write journal, pause envelope rewrite, `list_index` SQLite, event JSONL + `events.jsonl.commit` specialized. Missing `list_index` must not empty `task_records`. |
| Parquet datasets | `events`, `memory_events`, `activity_rows`, `activity_rollups`, `llm_calls`, `llm_embeddings`, `llm_provider_attempts`, `llm_tool_calls`, `llm_capture_gaps`, `llm_dispatch`, `llm_call_io`, `llm_context_blocks`, `llm_content_tombstones`, `llm_content_access_audit` | Local `dt=*` (activity `dt=*/hour=*`) canonical; reads via `family_read_glob` / `family_parquet_glob`. Compaction, checksum, retention: `parquet_maintenance`; `analytics_duckdb` is a regenerable catalog. |

**Subprocess and skill storage.** `workdirs/` and
`workdirs/skill-working/{call_id}/` are ephemeral until accepted. The governed
launcher writes a versioned `SubprocessStorageEnvelope` (scratch lease, input
manifest, SecretRef requests, output slots, result manifest) without replacing
an admitted CLI working directory or a `WorkingDirectoryMode::Denied` contract.
Closed children get no `MAGICIAN_ROOT_DIR`, `MAGICIAN_STORAGE_PATH` or store
credentials; accepted slots publish through the object kit, the rest is scavenged. Reader: `subprocess_owners::load_envelope` (`ENVELOPE_VERSION`,
`ENVELOPE_FILE_NAME`). `skillshub/scripts/runtime_root_shim.py` maps old
runtime-root paths (deprecation metric); `scripts/skillshub_runtime_root_linter.py`
runs in the `scripts/docs_guard.py` gate.

**Desktop engine-path reads.** Wake-word models, OS permissions and Application
Support config are device-local or bootstrap; engine data goes only through
`engine_base_url` (`None` → `http://127.0.0.1:{magician_port}`).
`is_remote_engine()` skips local container supervision and refuses engine-owned
file reads (device automation may be unavailable).

## Live workspace I/O

- `ArtifactV2Workspace::write_path` / `write_atomic_path` / `read_path` classify
  the locator and persist through owner kits; scratch, ephemeral and notes paths
  stay on the file provider. DuckDB `COPY TO` parquet goes through
  `publish_written_parquet` after the durable rename (outside `scopes/` or a
  dataset family the rename is the publish); compaction locators include
  `dt=*/_compact/*.parquet`.
- `events.jsonl` and its `events.jsonl.commit` byte-length marker stay on
  `WorkspaceFileProvider` so no whole-file persist skips the fsync/rename
  uncertainty protocol. SQLite/DuckDB engines `Connection::open` on
  `database_file_path` / `host_database_path` (local `domain_repository`).
  Device-local iMessage/WhatsApp ingest stays outside the kits; `StorageRuntime`
  object/dataset adapters stay unselected until Track B cutover.
- Run budget grants (`budget_grants.json`, beside `engine_pin.json`) are an
  owner-written sidecar in the execution dir, appended by the shared durable
  writer (unique staging file, file fsync, atomic rename, parent-dir fsync); a
  write invalidates the grant cache, the running loop otherwise re-reads on
  mtime/length change, and kept out of the authority-hashed
  pause record ([run budgets](../magician-api/README.md#run-budgets)). Boot
  (`magician-bin`) publishes `execution.budgets` before execution loops start.
- SaaS-split seams: `api_scope::ResolvedScope` yields `magician_storage::ScopeId`
  for durable-artifact REST and realtime WS; `DeviceTransport::LocalLoopback`,
  installed at boot, is read by browser CDP.

## Typed capabilities

The catalog names each owner's intended capability:

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

Switching the Settings Runtime provider moves no data. `local_file` and
`silverbullet_space` are variants of one layout; `silverbullet_space_runtime` is
cataloged so it cannot be deleted as a "ratchet".

## Recovery, activation, and profiles

Recovery (`magician/src/magician_v2/recovery/`) snapshots via each kit's
`export_all`; restore onto a fresh host re-imports objects, datasets, then
repositories, walks object version/digest and dataset parts, and rebuilds
indexes on an empty scratch/index root. Archives are integrity-signed and vault
bytes stay out of backups; object tombstones keep bytes seven days, GC never
collects a referenced version or the live replacement generation, dataset GC
drops generations absent from the current manifest. See
[magician-storage gates](../magician-storage/README.md#gates) and the
[recovery ADR](../magician-storage/adr-2026-09-01-recovery-objectives.md).
Credential rotation rewrites no product bytes; default startup neither snapshots
nor collects garbage.

Default profile `local_embedded` needs no bootstrap file. Every Tier 2 owner has
a rebuild or unavailable contract in
[capability-support-matrix.yaml](../magician-storage/capability-support-matrix.yaml);
deleting scratch and indexes drops no Tier 1 bytes. A 900 s offline cutover
freeze is accepted, so online migration is optional
([adr-2026-09-01-performance-capacity.md](../magician-storage/adr-2026-09-01-performance-capacity.md)).

Operator workflow: `magician storage inventory|plan|export|import|verify|cutover|rollback|status`,
`GET /api/magician/v2/storage/activation`,
`POST /api/magician/v2/storage/activation/cutover`. Cutover needs Decision Gate
3, a recent backup, remote health, no competing lease, rollback retention and
the confirmation `CUT OVER STORAGE`; Settings and the command palette show
inventory and status only ([task-19-activation.md](../magician-storage/task-19-activation.md)).

93 owners, 76 tier 1; 89 are `remote_ready`. Activation needs every tier-1
owner `remote_ready`, so tier-1 `relentless_runs` (`characterized`) and
`relentless_guidance_policies` (`discovered`) block it; tier-2
`decision_metrics_sqlite` (`discovered`) and
`automatic_database_maintenance_status` (`characterized`) do not. Coordinator
drills use a test-only fixture inventory that neither qualifies a store nor
alters canonical readiness. Profile acceptance
(`magician/src/magician_v2/profile_acceptance/`): browser, screen, meeting and
audio are `device-bridge-required`; the four device-local owners (`notes_provider`,
`desktop_engine_roots`, `device_local_imessage`, `device_local_whatsapp`) are
`unavailable` under `remote_durable`.

## What a scope is for

Any write runs `create_dir_all` on its parent, so merely *looking up* a scope
materializes it. `ScopeProfile` (`artifact_v2/workspace.rs`) draws the line:

| profile | scopes | meaning |
| --- | --- | --- |
| `User` | everything else, default scope included | a tenant; any subsystem may materialize |
| `ReservedSink` | `system`/`system`, `_quarantine`/`_quarantine` | records of no tenant: no owner, surface or inbox |
| `Invalid` | `system`/anything other than `system` | not a scope (below) |

A test asserts the three enumerations agree with their tenant-only
counterparts: `list_scopes()` / `list_tenant_scopes()`,
`list_scope_segments_sync()` / `list_tenant_scope_segments_sync()`,
`list_scope_segments()` / `list_tenant_scope_segments()`.

`scope_hosts_user_subsystems()` stops tenant subsystems (apps, mail, UI feed and
threads, social, api-mining) from materializing in a sink; event and analytics
state and listing are never gated. Eager sweeps (artifact verification, app boot
admission and slot defaults, content-source observation, agent-memory
resolution, recipe verification, vibedev runs, taste capture, feed and
ui-threads materializers, attention historical bootstrap, actionability
training, the two `magician-comms` `background_scopes` helpers) enumerate
tenants only; analytics and the transport-log stream keep `list_scopes()`.
First-touch residue in a sink (lock files, empty catalogs) is accepted; to find
the opener, measure the file handles the process holds or log first open (e.g.
`registry_connections()`), not enumeration call sites. The predicate is
deliberately narrow: `anonymous`/`default` is the single-user tenant (widening
would ship a deployment with no apps); tests pin it, `anonymous`/`system` and
other principals' workspaces as tenants.

**The `system` principal owns one workspace.** Any pair other than
`system`/`system` (`system`/`default`, `system`/`notes`) is `Invalid`, defined
once in `transport_log::is_invalid_system_scope`. `list_scopes()` and
`list_scope_segments[_sync]()` omit it (so sweeps including
`list_definition_scope_keys` skip it), agent templates never materialize there,
and writes land in `_quarantine`/`_quarantine` (`sanitize_scope_pair` for the
transport log, `quarantine_invalid_system_scope` for LLM-call, embedding and
activity-row parquet). On-disk maintenance (parquet compaction, dispatch-row
retention, LLM trace journal recovery) uses `list_all_scope_dirs[_sync]()` and
still sees such debris.

**Deleted workspaces are not scopes either.** `retain_live_scopes` (behind
`list_scopes()` / `list_scope_segments[_sync]()`) drops a scope whose
principal's readable `workspaces.json` does not vouch for it
(`auth::workspace_registry::is_retired_workspace`; one `stat` per call, parse
cached by mtime/len/inode), so workers skip a purged directory still on disk
before the next start; `list_all_scope_dirs[_sync]()` stays unfiltered. Shared lazy creators
refuse to create a *missing* root for a retired workspace
(`guard_scope_dir_creation` in `AgentStorage::ensure_base_layout`, mail store,
embeddings sink); database-sourced scope lists (attention learning,
resurfacing) drop retired scopes.

Liveness has one source, `scope_standing`; `is_live_workspace`,
`is_retired_workspace` and `enumerate_live_scopes` derive from it:

| Standing | Meaning |
| --- | --- |
| `Sink` | reserved sink pair |
| `Live` | registered and not queued for purge, or the default workspace of a principal with a registry or a `default` directory |
| `Deleted` | tombstoned; the only state destructive retirement acts on (`is_deleted_workspace`) |
| `Unlisted` | absent from a readable registry: skipped, never deleted |
| `Unregistered` | no registry: not live, not retired |
| `Unknown` | unreadable, empty or implausible registry: fails open |
| `NotAScope` | invalid names, `system/<other>`, reserved principal off its sink pair |

A stray directory or `_quarantine` is never handed a `default` workspace.

## Typed storage boundaries

`scripts/check_typed_storage_boundaries.py` (`make check-typed-storage-boundaries`)
rejects new implicit-path storage; allowlist only reviewed adapters, owner kits,
backup/export, the `magician-bin` composition root and remaining
`default_storage_base_path` sites. `remote_ready` deletes no local source;
copying a scope directory is not a migration
([task-21-typed-storage-boundaries.md](../magician-storage/task-21-typed-storage-boundaries.md)).
Raw `llm_calls` parquet reads stay in the analytics tree; the boot
planning-usage backfill goes through `analytics::planning_usage_ledger` and
`EventLog`, so the composition root opens no storage itself. Track A reaches `remote_ready` with no remote I/O; Track B is an
explicit operator cutover after Gates 1–3.

## Guard and docs

[Storage Governance](storage-governance.md) (operator surface),
[Workspace File Provider](workspace-file-provider.md), guard
`scripts/storage_catalog_guard.py` (tests `scripts/test_storage_catalog_guard.py`).

## Published process chat store

`magician-bin` publishes the indexed chat store once
(`chat::storage::publish_global_chat_store`) for read owners without server
wiring. Read-only (mutations go through the owning store); first publication wins (late ones logged); absent is
fail-closed (`global_chat_store()` → `None`, readers refuse). Example:
`meetings_data` polls via index-only
`ChatStore::list_thread_summaries_for_prefix` ([meetings-surface.md](meetings-surface.md)).

## Town Square corpus authority

The `town-square` app entity store is the corpus **authority**
([town-square-app.md](town-square-app.md)); `/social/*` uses it. `social.db` is a
migration source only: opened, governed and pruned, served to no user; disable
preserves no substrate.
`town-square-migrate` is the only bridge (load-bearing per-table
count-and-digest proof; history-only mode writes through the scoped Apps
entity-store owner, verifies with paginated read-back proofs, keeps the source
and never rewrites live membership or control records).
`spend_log` and `social_budget` are no longer created or retained, with no
`DROP TABLE`; never `SELECT` them; `prune_history` ignores `max_spend_rows`.
`SocialStoreRegistry` has exactly two readers (storage governance,
`town-square-migrate`) and must outlive the migration on every deployment (another reader would report
stale rows as healthy).

## Definition-store handle sharing

`AgentDefinitionStore` carries an `Arc<DefinitionCache>` invalidated only by
writes through that handle, so boot shares one `Arc<AgentDefinitionStore>` with
the `agent_roster_data` and `memory_data` binders (`tasks_data`, `notes_data`
hold none); out-of-band YAML edits are invisible. `for_scope` is a no-op without
a workspace layout, so scope-owned readers must confirm the re-point (else they silently read the
wrong scope). Materialization skips an invalid template (validation,
definition or agent-id mismatch) with a warning; I/O and other errors still fail. Read-only projections use
`list_definitions_shared` (`list_definitions` deep-copies).

## System-package boot admission

Contract: [system-package boot admission](system-boot-admission.md).
`distribution: system` packages come only from the read-only seed root
(`<seed>/system/<name>/app/`, at most 256 entries; seeded today: `meetings`,
`town_square`, `claims_review`, `learning`, `thinking_map`), admitted by
`admit_system_packages_at_boot` (`app_platform.system_packages.admit_at_boot`).
Staging and publication refuse a system manifest without that admission, so a
seeded package is unreachable until it runs.
Admission covers live workspaces only
(`auth::workspace_registry::enumerate_live_scopes` minus `pending_purge`;
unvouched directories are logged, never admitted or deleted). Each principal's
`default` is admitted before the app projection worker spawns; other
workspaces are admitted in the background and stay **pending** (the worker
skips them; on release their request-path caches are evicted).
Failed reviews persist in `scopes/<p>/<w>/apps/boot-review-failures.json`,
keyed by an input digest and not retried at boot until it changes; a purge
removes them.
`ArtifactV2Workspace::system_seed_root()` is canonicalized for
`admit_package_directory`; packages beneath go to the `O_NOFOLLOW` walker as-is.

## Task directory husks

`acquire_task_write_guard` creates the task directory and `.artifact_v2.lock`
before a write is known, so `with_task_write_lock` silently discards a lock-only
directory when its op errors (only while the exclusive guard is held). Locks
stay in the task directory so old and new binaries share one path in a rollout.
The planning-catalog bootstrap skips husks (debug log) and still marks the scan
complete; an unreadable state file defers the completion marker.

## Runtime env files

`magician-bin` loads `$MAGICIAN_ROOT_DIR/.env.development` and `.env` in `main`
before config load and any credential read, via
`workspace::runtime_config_path`; nothing upstream does
(`magic-supervisor` forwards its environment, `scripts/run-supervisor.sh` sources nothing).

## Evidence completion journal

Evidence decisions append to a densely numbered journal (512 entries per
segment) **after** the register row; every decision path also journals on replay
(idempotent by decision id), repairing a crash in between. Journal-first would
announce completions that never happened; the cost is that a failed append
reports an error although the row landed, and the retry returns
`already_applied` and repairs the journal.

## HITL lifecycle

- **Resolution is terminal per correlation id.** Requests against a resolved
  record are *invalid*, not reopens; a journal ending in a request replays to
  `Resolved`.
- **Durable authority outranks in-memory health.** The lifecycle journal commits
  before registry, canonical and broadcast visibility;
  `hitl_lifecycle_recovery_healthy` guards only the no-authority fallback and
  authority creation; it never suppresses a durable answer.

### Generation-bound app resolution stays side-effect free

With the V3 lifecycle authority ready, `emit_hitl_lifecycle_if_accepted` refuses
app-owner request/resolution events before taking publication authority; only
generation-bound reconciliation APIs advance them. A tokenless resolution may
not wait out a live ticket's cross-process lock and mark `Resolved` via
append-failure settlement; the refused call leaves the pending proof and
unconsumed ticket for the exact durable UserRequest owner. App runs classify by
the reserved canonical task identity; new runs use the internal task root, legacy app runs stay
in place, project as internal, and are included in rebuild and reconciliation so
orphan sweeps cannot erase them.

## Envoy Claims Review

- **Ingestion** serializes retries by transcript key; a retry must match the
  recorded act (sender, recipients, channel, work references, consequence class)
  before adding candidates, even after a partial import. Candidates bind
  relationship, occurrence time, extractor and source segment, so a changed
  import needs a new key; replay binds the source segment even if the claim
  reference changes, so a rejected statement cannot requeue nor a review
  decision duplicate. New imports first append an `ingestion_prepared`
  fingerprint (full source and utterance metadata, retry time excluded) to the
  scoped claim log before the act or any candidate; all candidates validate
  before any publishes, duplicates refuse the batch, fingerprint-less imports
  are checked against every act and candidate and bind on first valid
  resumption, and identical retries keep earlier review decisions. An owner can
  recover an unfinished confirmation's saved command after losing browser
  state (claim lock, revision and request-fingerprint check, no write).
- **Reply dispatch** reuses the scoped outward-assertion and transcript-claim
  stores and serializes on per-message locks under the workspace scope
  (`envoy_delivery_locks`);
  an uncertain attempt never authorizes a resend. Each lock has an atomic
  `<message-key>.json` binding (attempt UUID, address, payload digest; no reply
  text) written through the workspace storage owner before `dispatching`;
  missing claim preparation or a different binding never grants dispatch ([Claims Review](../unified-ui/claims-review.md)).

## Other scoped owners

- **Decision metrics**: scoped SQLite at
  `ArtifactV2Workspace::analytics_decision_metrics_path`; sampled review
  projections are restricted, retained incrementally, never replayed at startup.
- **Conversational source repair** uses `ChatStore` and `AgentStorage`: a bounded
  snapshot under the session lifecycle fence, revision check under the episode
  lock (shared with forget/retention), publish via `write_json_atomic`; the derived index rebuilds from journaled
  canonical changes; never creates a missing episode or parallel store ([conversational recall](memory-recall.md)).
- **Quality allowance continuation** appends an owner-approved reset to the run
  journal under its lock, keeping contract, ledger, paid checkpoints and cumulative
  usage; notices use the durable request owner and canonical receipt path; no
  parallel budget store. A run awaiting its owner keeps the active-root pointer,
  so its exact question or budget approval resumes the same run and no
  replacement root takes over.
- **Work-modules bridge**: registered with the host listener port at startup;
  in-app calls use the authenticated owner routes under the runtime
  principal/workspace with a per-call in-memory credential revoked on completion
  or cancellation; the reviewed route table grants no submit/sweep path; module
  records and reply Work use existing scoped stores only.

## Testing notes

### Workspace root in test harnesses

`MagicianV2Orchestrator` takes `durable_artifact_workspace` from
`services.workspace_layout`, else `resolve_scoped_root(runtime_config.storage_path())`
— the **seed** root (`config.storage_path` defaults to `"magician_data_v3"`; `resolve_scoped_root` is the identity),
not the runtime root (`resolve_storage_workspace`: env override, else
`$HOME/MagicianNotes`). Production passes `.with_workspace_layout(storage_workspace)`
in `magician_core/builder.rs`; `build_test_orchestrator_inner_at` must too, or
service and orchestrator disagree on where scope state lives.
`MAGICIAN_ROOT_DIR` still wins in a harness, so first-use sweeps check
`workspace::running_under_cargo_test_harness()`, not `cfg!(test)`.

<a name="activating-a-test-scope-for-the-stateless-driver-2026-09-04"></a>

## Fixtures run on the stateless driver (2026-09-04, seal retired 2026-10-08)

Fixtures exercise the default stateless driver with no seal or activation step
(runtime writer floor).
**Do not set** `MAGICIAN_EXECUTION_DRIVER=inprocess` to pass a test: `Inprocess`
is never implicit, lacks the durable outbox drain and has different restart
guarantees.

### Fixture rules

- **Canonicalize symlinked temp roots; never relax `SQLITE_OPEN_NOFOLLOW`.**
  macOS `temp_dir()` (`/var` → `/private/var`) makes the HITL authority fail with `unable to
  open database file`; use `canonical_tempdir` (`magician-api`) or
  `canonical_workspace_tempdir` (`realtime_events`).
- **Registry migration fixtures must be real**: build old `user_version`
  databases from the per-version constants (`APP_REGISTRY_SCHEMA` …
  `APP_REGISTRY_SCHEMA_V34` in `apps/registry.rs`), never relax a
  migration (`IF EXISTS`), and include the `app_registry_scope` row (else
  `ScopeCollision`).

### Rust pitfalls in the app-platform SQLite paths

- **`MappedRows` in a block tail.** `query_map`/`query` iterators borrow the
  statement, so `stmt.query_map(..)?.collect::<Result<Vec<_>,_>>()?` as a block
  tail is E0597 (the `?` temporary outlives `statement`); bind the iterator
  first (`query_row`/`execute` are unaffected).
- **`execute_scoped_typed_*` error inference.** Generic over `<T, E, F>` with
  `E: From<AppRegistryError>`, so mixed `?` error types hit E0283; annotate
  the closure (`move |connection, _| -> Result<T, MyError> { … }`).
  `F: 'static`, so closures reading `now` must `move`.

## Media voice choices

Scoped `media/preferences.json` stores optional `tts_voices` and
`realtime_voices` maps alongside existing surface selections. Schema 3 loads
older files with empty maps; partial writes use the existing scope-owned lock
and atomic ArtifactV2 writer. Runtime provider catalogs do not become global
voice preferences: each synthesis/call captures the requesting scope's choice
outside the shared provider cache. Phone-native voice IDs stay device-local.
