# Owner-closure reference packet

How a storage owner is closed, and where every cataloged owner's kit and
local layout live. Invariants for every owner below:

- Every catalog owner is `remote_ready` with `local_active` authority; local
  layouts stay canonical until an explicit migration. Default startup stays
  `local_embedded`: it never uploads, selects a remote adapter, invokes the
  migration coordinator, or cuts over.
- Remote stores and migration handlers in each kit are test-only. There is no
  S3 in magician.
- Tier 2 and device-local outcomes:
  [capability-support-matrix.yaml](capability-support-matrix.yaml).

The reference owner is `delivery_receipts` (the sent-index JSONL map); copy its
shape for new owners.

## What to copy

| Piece | Where |
| --- | --- |
| Catalog row | `docs/components/magician/storage-catalog.yaml` id `delivery_receipts` |
| Current contract tests | `magician/src/magician_v2/delivery_receipts/characterization.rs` |
| Typed boundary | `SentMessageStore` in `delivery_receipts/store.rs` |
| Legacy local adapter | `SentMessageIndex` (JSONL under `delivery/index/sent/`) |
| Remote adapter | `SqliteSentMessageStore` via `open_sqlite_sent_index` (explicit, tests only) |
| Scenario factory | `open_local_sent_index` / `open_sqlite_sent_index` |
| Caller injection | `open_local_sent_index(workspace)` at API, puller, and hygiene tests |
| Source guard | `delivery_receipts/guard.rs` — production `SentMessageIndex::new` stays in the adapter |
| Migration handler | `SentIndexMigrationHandler` + the fenced migration coordinator |
| Shared scenarios | `delivery_receipts/storage_packet.rs` |

## Sequence later owners must keep

1. Characterize current layout (restart, corruption, scope, idempotency/CAS).
2. Introduce the trait and keep the local adapter unselected by a new default.
3. Route every production caller through the factory; enable the owner guard.
4. Add the remote adapter and run the same scenarios on both.
5. Qualify export/import, interruption/resume, semantic verify, rollback, and
   fresh-host restore. Rollback must enter `RollbackPending` before clearing
   forward evidence, otherwise `enter_phase` reports missing cutover evidence.
   Advance catalog evidence to `remote_ready`.
6. Do not activate remote as part of closure.

Maildir bounce ingest stays local mailbox files. It is not a second
canonical map and is not migrated by this packet.

## Object owner — `durable_artifacts`

| Piece | Where |
| --- | --- |
| Catalog row | id `durable_artifacts` |
| Current contract tests | `magician/src/magician_v2/artifacts/characterization.rs` |
| Local adapter | `DurableArtifactStore` under `durable_artifacts/` (same paths) |
| Factory | `open_local_durable_artifacts` |
| Remote adapter | `ObjectDurableStore` over `ObjectStore` (hermetic local objects in tests; no S3 in magician) |
| Guard | `artifacts/guard.rs` |
| Shared scenarios | `artifacts/storage_packet.rs` |

## Remaining object owners

Shared kit in `magician/src/magician_v2/object_owners/`. Local directory
layouts stay canonical. `ObjectTreeStore` and `ObjectOwnerMigrationHandler`
are test-only over `magician-storage::LocalStorage`.

| Catalog id | Local layout |
| --- | --- |
| `app_packages` | `apps/packages/` (fd-pinned staging remains the writer) |
| `app_attachments` | `apps/attachments/` |
| `app_exports` | `apps/exports/` |
| `app_captures` | `apps/captures/` |
| `app_evaluations` | `apps/evaluations/` |
| `task_outputs` | `tasks/{id}/outputs/` and `internal_tasks/{id}/outputs/` |
| `execution_outputs` | execution `outputs/` including accepted tool results |
| `execution_observations` | execution `observations/` (screenshots / SoM) |
| `execution_downloads` | execution `artifacts/downloads/` |
| `execution_recordings` | execution `recordings/` |
| `llm_trace_journal` | `analytics/llm_trace_journal/` |
| `llm_restricted_journal` | `analytics/llm_restricted_journal/` |
| `skills_scope` | `skills/` accepted packages |

Completed downloads and execution recordings are `remote_ready` with local
layouts still canonical. In-flight `.part` files and live CDP screencast are
not durable. Notes-provider audio stays on the notes provider. ScreenshotStorage I/O and completed
download copies go through `store_for_any_owner` / `store_for_existing_path`.
Recordings persist only via `persist_execution_recording`.

## Parquet families

Shared kit in `magician/src/magician_v2/dataset_owners/`. Local `dt=*`
layouts stay canonical. `RemoteFamilyStore` and
`DatasetFamilyMigrationHandler` are test-only over
`magician_storage::LocalDatasetStore`.

| Catalog id | Local layout |
| --- | --- |
| `events` | `analytics/events/dt=*/*.parquet` |
| `memory_events` | `analytics/memory_events/dt=*/*.parquet` |
| `activity_rows` | `analytics/activity_rows/dt=*/hour=*/*.parquet` |
| `activity_rollups` | `analytics/activity_rollups/dt=*/*.parquet` |
| `llm_calls` | `analytics/llm_calls/dt=*/*.parquet` and `dt=*/_compact/*.parquet` |
| `llm_embeddings` | `analytics/llm_embeddings/dt=*/*.parquet` |
| `llm_provider_attempts` | `analytics/llm_provider_attempts/dt=*/*.parquet` and `dt=*/_compact/*.parquet` |
| `llm_tool_calls` | `analytics/llm_tool_calls/dt=*/*.parquet` and `dt=*/_compact/*.parquet` |
| `llm_capture_gaps` | `analytics/llm_capture_gaps/dt=*/*.parquet` and `dt=*/_compact/*.parquet` |
| `llm_dispatch` | `analytics/llm_dispatch/dt=*/*.parquet` |
| `llm_call_io` | `analytics/llm_call_io/dt=*/*.parquet` |
| `llm_context_blocks` | `analytics/llm_context_blocks/dt=*/*.parquet` |
| `llm_content_tombstones` | `analytics/llm_content_tombstones/dt=*/*.parquet` |
| `llm_content_access_audit` | `analytics/llm_content_access_audit/dt=*/*.parquet` |

DuckDB `COPY TO` sinks, compactors, rollups, reprice, and the LLM fact
materializer republish the durable object through
`publish_written_parquet` / `DatasetAccess`. Compaction still writes a
unique staging sibling, `fsync`s, and renames; the kit republish is the
typed boundary, not a second layout. Compatibility readers keep selecting
current local files until explicit migration.

## Work spine

Shared kit in `magician/src/magician_v2/work_owners/`. Local task,
execution, plan, pause, and list-index layouts stay canonical.
`RemoteWorkStore` and `WorkOwnerMigrationHandler` are test-only.

| Catalog id | Local layout |
| --- | --- |
| `task_records` | `tasks/{id}/` manifests and state (not outputs/executions/plans) |
| `internal_tasks` | `internal_tasks/{id}/` |
| `task_plans` | `{tasks,internal_tasks}/{id}/plans/` |
| `executions` | `tasks/`, `internal_tasks/`, and scoped `executions/` records (not object-owner dirs) |
| `task_planning_recovery` | `task_planning_recovery/` |
| `pause_states` | `runtime/pause_states/` |
| `runtime_resume_recovery` | `restricted/runtime_resume_recovery/` |
| `restricted_hmac_catalogs` | `restricted/{accepted_runtime_launch,execution_routing,plane_attenuation,operator_steers,manual_resume_transactions,delegated_child_recovery,execution_pipeline_rosters,execution_pipeline_progress,pipeline_terminal_settlements,delegation_round_progress,llm_capture}/`. Unlisted `restricted/*` stays unowned. |
| `list_index` | `ui/indexes/list_index.db` (rebuildable; must not hide empty truth) |

`FileV2Store` publishes `execution.json` through `persist_work_file` /
`store_for_any_owner`. The task multi-write journal, pause envelope
rewrite, and live `list_index` SQLite stay specialized local adapters.
Monitors are scheduled tasks on `task_records`.

## Chat and progress

Shared kit in `magician/src/magician_v2/chat_owners/`. Local session,
message, transcript, turn-event, enrollment, and progress layouts stay
canonical. `RemoteChatStore` and `ChatOwnerMigrationHandler` are
test-only. `ChatStore` / `FileChatStore` is not the
remote boundary; characterization is of the current file layout.

| Catalog id | Local layout |
| --- | --- |
| `chat_sessions` | `ui/chat_sessions/{id}/session.json`, intents, `.lifecycle/` |
| `chat_messages` | `ui/chat_sessions/{id}/messages/{segment}.jsonl` |
| `chat_transcripts` | `ui/chat_sessions/{id}/llm_history/` (manifest is commit point) |
| `chat_turn_events` | `ui/chat_turn_events/{chat_turn_id}.jsonl` |
| `chat_enrollments` | `chat/enrollments.json` |
| `ui_preferences` | `ui/preferences.json` |
| `media_preferences` | `media/preferences.json` |
| `progress_channels` | `progress_channels/events/{log_key}.jsonl` |
| `progress_subscriptions` | `progress_channels/subscriptions.json` |
| `progress_lineage` | `progress_channels/lineage_index.json` (rebuildable) |

`FileChatStore` publishes `session.json` through `persist_chat_file` /
`store_for_any_owner`. Progress maps use the same factory. JSONL appends
and transcript segment+manifest commits stay specialized local adapters.
Session `outputs/` stay on the session tree and are not a second
canonical map in this packet.

## Agents, memory, and learning

Shared kit in `magician/src/magician_v2/agent_owners/`. Local definition,
runtime, canonical memory, learning, and index-file layouts stay
canonical. `RemoteAgentStore` and `AgentOwnerMigrationHandler` are
test-only. Path-returning `MemoryStorage` stays the
local adapter.

| Catalog id | Local layout |
| --- | --- |
| `agent_definitions` | `agent_runtime/agents/{id}/definition.agent.yaml` and `definitions/` |
| `agent_runtime` | approvals, proposals, scheduler, per-agent `state/` |
| `memory_canonical` | `memory/` except `index/`; legacy `agent_runtime/agents/{id}/memory/` |
| `memory_index` | `memory/index/` (manifest, documents, LanceDB files) |
| `learning_procedures` | `learning/procedures/` except `index/` |
| `learning_procedure_index` | `learning/procedures/index/` (rebuildable) |
| `learning_histories` | `learning/{events,candidates,decisions,evaluations,skill_invocations}/` |
| `skill_evolution` | `skill_evolution/` (legacy `capability_evolution/` learning rows merge-read only) |

`AgentStorage` publishes classified files through `persist_agent_file`.
`LearningStore` uses `persist_agent_file_sync`. JSONL appends and live
LanceDB stay specialized local adapters. A deleted LanceDB directory
must not empty canonical memory.

## SQLite and DuckDB lifecycle stores

Shared kit in `magician/src/magician_v2/database_owners/`. Local database
files stay canonical. `RemoteDatabaseStore` and
`DatabaseOwnerMigrationHandler` are test-only closed-file snapshots. Live `Connection::open` / DuckDB stay specialized.

| Catalog id | Local layout |
| --- | --- |
| `analytics_duckdb` | `analytics/analytics.duckdb` (regenerable from Parquet) |
| `channel_assist_duckdb` | `mail_assist/mail_assist.duckdb` |
| `ui_threads_duckdb` | `ui/threads/ui_threads.duckdb` |
| `feed_duckdb` | `ui/feed/feed.duckdb` (regenerable) |
| `browser_engine_usage_sqlite` | `analytics/browser_engine_usage.sqlite3` |
| `app_store_sqlite` | `apps/app_store.sqlite3` |
| `social_sqlite` | `social/social.db` |
| `api_mining` | `api_mining/projections/_rows.db` and projection JSON |
| `attention_learning_sqlite` | `{runtime_root}/attention_learning.db` |
| `attention_funnel_sqlite` | `{runtime_root}/attention_funnel.db` |
| `resurfacing_sqlite` | `{runtime_root}/resurfacing.db` |
| `hitl_lifecycle_sqlite` | `{runtime_root}/pending_hitl_lifecycle.v3.sqlite3` |

Production paths go through `database_file_path` / `host_database_path`.
Live engines still `Connection::open` on those kit-owned files (WAL/pages
cannot be `ObjectStore::put`). Channel Assist, API mining projections,
and the attention-funnel host DB resolve that way at open and in the
`/storage` inventory. Device-local iMessage/WhatsApp databases stay classified in
the system/devices section and are not opened through this kit. Scratch DuckDB is ephemeral.

## System, devices, and secrets

Shared kit in `magician/src/magician_v2/system_owners/`. Local system,
tenant, and secret-metadata layouts stay canonical. `RemoteSystemStore`
and `SystemOwnerMigrationHandler` are test-only.
Secret export is metadata-only.

| Catalog id | Local layout |
| --- | --- |
| `secret_vault` | `secrets/secret_audit.jsonl` (metadata); vault bytes stay local |
| `resource_authority` | `resource_authority/` |
| `programs` | `programs/` |
| `capability_evolution` | `capability_evolution/` (capability-pack catalog; not Skill Evolution) |
| `evals_runs` | `evals/runs/` |
| `content_sources` | `content_sources/` |
| `transport_log_jsonl` | `events.jsonl` |
| `compaction_metrics` | `storage_governance/compaction_metrics.json` |
| `device_pairing` | `system/paired-devices.json`, `system/device-policy.json` |
| `system_templates` | `system/` templates excluding pairing files |
| `runtime_config` | host `magician-config.yaml` (bootstrap) |
| `device_local_imessage` | device Messages DB; not migrated |
| `device_local_whatsapp` | device WhatsApp DB; not migrated |
| `notes_provider` | human notes; not canonical runtime |
| `silverbullet_space_runtime` | local profile variant |

## Desktop engine-path reads

Kit in `desktop/src-tauri/src/engine_roots/`. Device-local and bootstrap
paths stay on the desktop. Engine-owned paths are reached only through
`engine_base_url`. `is_remote_engine()` skips local container
supervision and refuses engine-owned filesystem reads. Classified, not
migrated as server tenant storage.

| Path id | Class |
| --- | --- |
| `wake_word_model` | device-local |
| `os_permission_state` | device-local |
| `desktop_application_support` | bootstrap |
| `desktop_env_file_override` | bootstrap |
| `engine_env_file` | engine-owned |
| `engine_tenant_tree` | engine-owned |
| `local_container_volume` | local-engine-only |
| `notes_space_placeholder` | bootstrap |

Desktop `setup.rs` may resolve the runtime root only as the selected local
container volume and setup display value; remote placement never reads engine
bytes through that path.

## Backup, restore, and GC

Kit in `magician/src/magician_v2/recovery/` plus delayed GC on
`magician-storage` local object/dataset adapters. `magician-bin` does not depend on `magician-storage-s3`,
`magician-storage-state`, or `magician-storage-migration`. Gate 2 numbers:
[adr-2026-09-01-recovery-objectives.md](adr-2026-09-01-recovery-objectives.md).

Representative restore owners: `programs`, `task_records`, `chat_sessions`,
`memory_canonical`, `agent_runtime` (schedules), `app_attachments`,
`events`, `device_pairing`, `secret_vault` (audit refs only). Drills cover
local_embedded backup/restore, remote logical export / SQLite PITR with
object-generation validation, fresh-host index rebuild, deleted-object
retain until unreferenced GC, credential rotation without rewriting product
bytes, and Gate 2 pass/fail comparison.

## Gates and activation

- Gate 3 budgets: [adr-2026-09-01-performance-capacity.md](adr-2026-09-01-performance-capacity.md)
  (kit `magician/src/magician_v2/gate3/` plus adapter drills in
  `magician-storage`, `magician-storage-s3`, `magician-storage-state`).
- Track B activation: kit `magician/src/magician_v2/storage_activation/`, CLI
  `magician storage …` and `GET /storage/activation`; cutover needs backup and
  remote health ([task-19-activation.md](task-19-activation.md)).
- Track A acceptance kit: `magician/src/magician_v2/track_a_acceptance/`;
  profile acceptance (compute/storage placement scenarios plus the
  device-bridge boundary): `magician/src/magician_v2/profile_acceptance/`.

## Process-wide typed runtime

Magician-bin installs `StorageRuntime` and the resolved workspace root.
Libraries use `magician_v2::process_storage`; the satellite list is in the
[README](README.md#process-wide-typed-runtime-task-22).

## Typed storage boundaries

`scripts/check_typed_storage_boundaries.py` is the program-wide bypass
ratchet. Owner-by-owner `SourceGuard` values still live in each kit.
Do not delete retained local adapters because an owner is `remote_ready`.
[task-21-typed-storage-boundaries.md](task-21-typed-storage-boundaries.md).

## Subprocess and skill storage

Shared kit in `magician/src/magician_v2/subprocess_owners/`. Local
workdirs and skill-working leases stay ephemeral until accepted.
`RemoteSubprocessStore` and `SubprocessOwnerMigrationHandler` are
test-only. Accepted outputs publish through the object
owners.

| Catalog id | Local layout |
| --- | --- |
| `workdirs_scratch` | `workdirs/` except `skill-working/` |
| `subprocess_skill_working` | `workdirs/skill-working/{call_id}/` |

The governed launcher installs `SubprocessStorageEnvelope`. Closed
children do not inherit engine roots or store credentials. Operator
batches that still need the live runtime root call
`skillshub/scripts/runtime_root_shim.py`. The skillshub linter rejects
new direct `MAGICIAN_ROOT_DIR` / `MAGICIAN_STORAGE_PATH` resolution
outside that shim.
