# Magician Bin

**Current development version:** `0.2.28`

Version 0.2.28 wires the scoped Work-module bridge and task service into
startup with Magician 0.7.107 and API 0.3.43.

The `magician` server binary and composition root, in its own package. A
satellite `magician-api` depending on the `magician` lib cannot live in the
lib's package (`magician → magician-api → magician`), so `magician-bin` depends
on both and nothing depends back on it. The binary name stays `magician`, so
`replace-release-bin.sh` and `make build-all-release` are unaffected.

## Shape

- `src/main.rs` — server wiring (actix bootstrap, service graph, CLI
  subcommands). Its `magician_api` handler imports (`chat_api`, `plane_api`,
  agent services) are kept rustfmt-sorted; reordering them registers no new
  route. `GET` and `POST /meetings/whatsapp-ring` are registered with the other
  literal `/meetings/*` routes, before `/meetings/{id}`.
- `src/adapters/` — local tool services.
- Depends on `magician`, `magician-api`, `runtime-core`, `tool-runtime-core`
  (through the root's exact MagicRun pin), `magicllm`, and CLI/HTTP deps;
  `once_cell` holds the process-lifetime cells installed at boot. Shared
  custody goes through Magician's MagicVault facades; bootstrap, keychain
  identities and runtime configuration stay product-owned.
- `magician-core` is a dev-dependency with `test-support`, because the
  execution harness seam's `turn_engine.rs` renders through the prompt store
  and a test binary never installs the global PromptManager
  (`make test-execution-harness`; see
  [execution harness conformance](../magician/execution-harness-conformance.md)).
- `decision-engine-contract` is a dev-dependency for
  `examples/planning_decisions_phase_d_eval.rs`, which reads engine replies
  (see the planning plan).

## Boot sequence

The supervisor task set owns one `idle-workspace-resources` collector. Every
15 seconds it submits an admitted blocking pass over weakly registered database
and connection caches, releasing unused owners without deleting workspace data.
It shuts down with the supervisor, before analytics/storage teardown. See
[idle workspace resources](../magician/duckdb-analytics.md#idle-workspace-resources).

Sealed task agents explicitly grant discovery, completion and input controls.
Their granted packs cover registered `<pack>__<action>` leaves in the shared
policy matcher; exact leaf denials stay narrow and unrelated prefixes stay
ungranted. Ambassador uses these controls with company-only calendar and
per-act write review; its universal and owner-personal tool boundaries stay
sealed.

1. **Runtime env files** — `$MAGICIAN_ROOT_DIR/.env.development` then `.env`
   load once in `main`, right after argument parsing and before config load,
   router construction or any credential read, on every path. `dotenvy` never
   overwrites an existing variable, so precedence is real environment >
   `.env.development` > `.env`. This matters because `magic-supervisor` and
   `scripts/run-supervisor.sh` source nothing. The loader is `Once`-guarded and
   records the keys it added (`set_runtime_injected_env_keys`) so Workbench
   terminals can drop them (see
   [Developer Mode Workbench](../magician/developer-mode-workbench.md#terminal-environment)).
2. Provider-free app authoring returns before config load.
3. **Storage bootstrap** — `--storage-bootstrap` /
   `MAGICIAN_STORAGE_BOOTSTRAP_CONFIG` are resolved by `magician-storage`
   before `magician-config.yaml` is opened; neither keeps the local
   `workspace_storage` profile. Live paths then load config and resolve
   `runtime.scale` (`MAGICIAN_SCALE_PROFILE` overrides the name) to build the
   main / execution / Lance Tokio runtimes. The same plan sets the live-agent
   limit, `per_agent_outstanding_limit` and `delegated_child_permits` before
   any agent starts.
4. After the workspace root resolves, boot installs one `StorageRuntime` under
   `.magician-storage/` **before** CLI / `--reindex` early returns. The
   exclusive default `anonymous`/`default` scope lease is taken only on the
   server path. Remote profiles fail closed at default startup; health is
   `GET /health/storage`. Satellites use the installed runtime, never
   `MAGICIAN_ROOT_DIR`; host SQLite opens through `host_database_path`. See
   [magician-storage](../magician-storage/README.md). Boot also installs
   `DeviceTransport::LocalLoopback` with the configured Magicutor CDP and
   Ollama embedding URLs.
5. **Jail cleanup (server path)** — removes this user's leftover
   `magician-app-credentials-<pid>-<uuid>` directories whose process is dead or
   not this one, then runs MagicRun's `sweep_stale_jail_members()` once to kill
   processes left behind by a process that died mid macOS exec-roots jail.
   Counts are logged (`[APP-JAIL]`); an unreadable sentinel is kept and
   warned. Then **workspace purge and retirement**:
   `workspace_registry::drain_pending_purges` removes directories of
   workspaces deleted with `?purge=true`, and
   `workspace_retirement::retire_host_state_before_stores_open` retires the
   state they left outside their directories before anything opens it
   (per-scope files such as taste capture, content cache and actionability
   snapshot; HITL lifecycle authority rows; the HITL delivery log; auth
   bearers). After HTTP readiness (so full-table scans never delay `/health`),
   a background task runs `workspace_retirement::retire_store_rows` to delete
   store rows of TOMBSTONED workspaces (attention learning, resurfacing,
   attention funnel, list index, paired devices, push registrations); worker
   scope lists already skip deleted workspaces. Only tombstoned ids (`deleted`
   / `pending_purge`) are retired; absence from the registry is not enough.
   Failures never block boot. Accounting (`llm_dispatch/*.jsonl`), audit
   (`system/device-actions.jsonl`) and SilverBullet notes are kept. See
   retiring a scope.
   A second background task starts the attention learning
   storage-maintenance timer (`AttentionLearningService::spawn_storage_maintenance`,
   supervised; a panic restarts the loop after 60 s; first tick after 2 min
   plus jitter, then every 15 min, scopes 5 s apart): the keep-latest
   projection prune and the gated scoped retention pass.
   A third, beside the feed materializer, prunes feed failure rows older
   than a week (`FeedStore::prune_failed_items`) at boot and daily, only in
   scopes that already have a feed database.
6. **LLM** — LLM journal recovery runs on the blocking lane beside App
   package admission; boot joins it before content capture, dispatch
   attachment or HTTP readiness, and recovery errors stop startup. Built-in
   chunk adapters register before the live operation router is built. When
   `llm.dispatch.enabled`, one `LlmDispatchQueue` is installed on both
   `OperationLlmRouter` and `MultiLLMService`, so operation calls and chat
   share provider caps, RPM/TPM and `global_cloud_concurrency` (with `false`
   both route directly). The queue uses
   `OperationLlmRouter::live_dispatch_router()`, so a config reload takes
   effect without restart. Provider registration skips an uninstantiable
   profile with a warning and fails only when *no* profile registered
   (`magicllm::bootstrap`).
7. **Coding-engine workers** — `spawn_codex_qualify_worker`,
   `spawn_grok_version_worker`, `spawn_claude_version_worker`,
   `spawn_agy_version_worker`, so HTTP never waits on `codex app-server` or
   `--version`. Grok overlays CLI version, filesystem/env auth and ACP
   initialize isolation (omitted MCP/tool lists stay unqualified). Claude
   overlays version, Max/OAuth and init-only isolation. Agy overlays version,
   Antigravity OAuth and init-only isolation (`init.tools` + `permission_mode`;
   identity is path + mtime + length).
8. **Attention training worker** runs when actionability, routing or bandit
   `training.enabled` is true. Shipped bandit YAML is `shadow` with no snapshot
   pin; Magician auto-installs a store snapshot and may promote it to canary.
9. **Auth** — boot opens `AuthStore` under the runtime root, attaches it as
   `AuthRuntime`, and wraps `/api/magician/v2` and `/v3` with
   `auth::middleware::authenticate_request` as the outermost gate (before
   Cloudflare Access; inert in `auth.mode: open` without a bearer; see
   [auth](../magician/auth.md)). Its 401 carries CORS headers.
   `magician_v2::cors::api_cors_middleware` wraps innermost on both scopes,
   granting the wildcard and answering every `OPTIONS` before routing, because
   a nested `web::scope` never falls back to a `/{tail:.*}` catch-all and would
   404 the preflight.
10. **Harness affinity** — last-driver-wins: the run engine is primary; when it
    is `magician`, the chat mouth's engine drives. The resolved name goes to
    `set_harness_affinity` as `Some(engine)` unconditionally (the getter
    filters the built-in, so this both sets and clears). Boot installs
    `install_chat_harness_snapshot` from `chat.harness_engine` (default
    `magician`) beside `execution.harness_engine`, and loads
    `install_llm_routing_overrides`.
11. **System-package admission** (startup phase `apps_and_history`, a
    `tokio::join!` with user-request history and the chat-store index) —
    startup awaits `AppPlatformApi::admit_system_packages_at_boot`
    (`distribution: system`: `meetings, town_square, claims_review, learning,
    thinking_map`) only for **each principal's `default` workspace**; that is
    all `/health` (503 `service_starting` until then) waits for. Scopes come
    from the workspace registry (registered workspaces + default, minus
    `pending_purge`), not from `scopes/` directories; unregistered directories
    are logged once and skipped. Every other live workspace is admitted by a
    background task after HTTP readiness (3 at a time) and stays *pending* for
    the projection worker until done, so nothing caches "not installed" for a
    half-admitted scope. On an unchanged boot, readiness costs an idempotent
    re-publication plus a status read per package; on a first boot or version
    bump, one review preparation per package. Staging and publication refuse a
    system manifest without this owner. See
    [system-boot-admission](../magician/system-boot-admission.md).
12. **Composition latch** — agent startup hydration spawns right after
    `stateless_loop_lifecycle_ready.cancel()`, beside the other lifecycle
    owners, because hydration re-arms autonomous goals and a due goal
    dispatching before `AgentRuntime`'s `artifact_v2_service` is wired burns
    the cycle (`record_failed_goal_cycle`). The ordering test asserts
    `opened < hydration`.
13. Startup recovery: the pause reaper removes records whose execution
    directory is gone, whose `state.json` never landed, or whose durable state
    is terminal (`pause_execution_record_is_gone_or_terminal`); a real I/O
    failure keeps the record. The synthesis reconcile sweep budgets each task
    itself (no outer timeout) and is idempotent.

**Planning usage backfill.** After the execution-panel event log is created,
boot spawns `planning_usage_backfill::spawn`, a one-off job that writes
planning model usage from the LLM call ledger onto past tasks' timelines (see
`docs/components/magician/plan-flow.md`). Nothing waits on it; failures are
logged.

## Execution driver and shutdown

Stateless is the default; `MAGICIAN_EXECUTION_DRIVER=inprocess` is an explicit
rollback. `GET /health/execution-driver` reports the resolved driver
(`ExecutionDriver::select`), any unrecognised value, and per-arm run counts
since start (`resolved_driver` should be `stateless` and
`runs_since_start.stateless` should advance). Shutdown keeps the wake
supervisor until claimed child handlers finish generation-bound handoff, then
closes the execution-job registry, refuses late launches, drains in-memory
futures, and only then tears down event, queue and storage sinks. Cancellation
here never fabricates a terminal execution.

## CLI subcommands

- `seal-stateless-loop-cutover` — **retired**; still parses for one release
  and does nothing. Startup enforces the runtime writer floor instead: a build
  older than `.magician-storage/runtime-writer-floor.json` records refuses to
  start (runbook note).
- `magician storage inventory|status|plan|export|import|verify|checkpoint|resume|cancel|cutover|rollback`
  — explicit operator workflow; cutover fails closed until every precondition
  holds ([task-19-activation.md](../magician-storage/task-19-activation.md)).
- `magician ollama-launch-config` — answers under `privacy.processing.mode:
  cloud` with `generation_model_count=0` and sizes the daemon context from the
  embedder, so `run-ollama.sh` never falls back to a locality-unaware
  resolver.
- `magician town-square-migrate` — one-shot migration of the retired
  first-party Town Square corpus into the `town_square` package's entity
  store; a CLI so it has no auth surface and is not network-reachable. It
  refuses without an enabled `app:town-square` installation and exits non-zero
  when the migration is not faithful (the exit status is the go/no-go for
  retiring the source). `--history-only` restores historical conversation
  entities into a used square, keeping live roster, moods, policy and cursor;
  no source data is deleted.
- The live `magician app` client covers candidate publication, reviewed
  lifecycle and re-enable, update/reinstall migration and rollback, purge
  recovery, and package/data/combined portability. It reads archive
  passphrases only from bounded no-follow files and never opens registry or
  staged-generation storage directly.

## Routes mounted here

Handlers often live in `magician-api`.

- `GET/PUT /plane/decision-routing` — owner-only Decision Engine routing.
- `POST /memory/search` — the owner Memory page, same recall as
  `search_memory`.
- `GET /components/catalog`, `POST /components/plan`, and setup-token-gated
  component installation job routes (probes, capability planning, serialized
  execution, post-install verification).
- `GET /edge/bridge` (paired Desktop Edge outbound WebSocket; hello/lease/
  generation protocol, server-side capability registry), `GET /edge/devices`,
  `POST /edge/devices/{device_id}/invoke` (typed dispatch with a server-minted
  grant).
- `POST/GET/DELETE /plane/mcp` — loopback MCP; bearer is a `plt_` grant;
  `DELETE` ends a streamable-HTTP session, never the grant; `tools/call` is
  governed dispatch when the grant has live executors
  ([the plane](../magician/plane.md)).
- `GET/POST /plane/grants`, `DELETE /plane/grants/{id}`; `GET /plane/engines`
  (roster, install status, `native_tool_posture`); `PUT /plane/engine`;
  `PUT /plane/chat-engine` (any launchable roster name).
- `GET/POST /vibedev/projects/{id}/crews`,
  `POST …/crews/{crew}/start|stop|integrate|resolve|choose|pull-request` —
  VibeDev crews ([magician-api](../magician-api/README.md#vibedev-run-list-paging));
  boot spawns `spawn_vibedev_crew_supervisor` once HTTP is up.
- `GET /vibedev/runs/{id}/turns`, `…/timeline`, `…/changes`,
  `GET /vibedev/repos/suggest` — mobile run detail
  ([magician-api](../magician-api/README.md#vibedev-run-list-paging)).
- `GET /vibedev/runs/live` — every open project's going, waiting and
  just-ended VibeDev runs, one state each
  ([magician-api](../magician-api/README.md#vibedev-run-list-paging)).
- `GET /work/live`, `POST /work/follow`, `DELETE /work/follow/{item_id}` —
  "Magician at work": coding runs, crews and Work tasks ranked for the phones
  and the notch, and up to three followed items (`work_live`).
- `GET /vibedev/runs/{task_id}/branch`, `GET …/disk`, `POST …/merge`,
  `…/pull-request`, `…/rewind`, `…/discard` — review and land a trusted
  VibeDev chain's branch (owner-only except the two reads;
  [magician-api](../magician-api/README.md#vibedev-trusted-repo-mode)).
- `GET/PUT /budgets/defaults`, `GET/POST /tasks/{task_id}/budget` — run
  budgets (`execution.budgets` caps, per-execution grants;
  [magician-api](../magician-api/README.md#run-budgets)). Boot publishes
  `execution.budgets` before the loops start.
- `GET/PUT /settings/privacy` (`privacy.processing.mode`, applied by reload);
  `GET/PUT /settings/local-generation` (pins
  `runtime.ollama.local_generation.selected`; RAM-tier warnings, then reload +
  `run-ollama.sh`).
- `GET /chat/invoke-grammar` (additive-only lane invoke grammar).
- `GET /hitl/{correlation_id}/alignment`, `GET/PUT /preference-alignment/policy`,
  `GET /preference-alignment/status`, `GET /preference-alignment/notices`,
  `POST /preference-alignment/notices/{id}/answer` — preference alignment
  owner surfaces (`configure_preference_alignment_routes`; the voice answer
  sink `ApiVoiceReviewSink` is installed beside the verification-code sink;
  [magician-api](../magician-api/README.md#preference-alignment-preference_alignment_apirs)).
- `GET /llm/routing`, `PUT/DELETE /llm/routing/{operation}`,
  `PUT/DELETE /llm/routing/{operation}/engine`.
- `POST /contextual-writing/actions`, `POST /contextual-writing/sessions`,
  `GET /contextual-writing/catalog`
  ([contextual-writing](../magician/contextual-writing.md)).
- `POST /coding/engines/{codex_app_server,grok_acp,claude_code,agy_cli}/refresh`
  (202).
- `GET /media/providers` advertises each realtime profile's voice catalog.
  With `GEMINI_API_KEY` present, boot registers Gemini 3.5 Transcribe
  (`gemini_transcribe`) for Dictation file STT and Gemini 3.5 Live Transcribe
  (`gemini_live_transcribe`) for Hands-free streaming STT.
- `POST /chat/memory/sources/repair` — scoped memory source assessment and
  repair, over the published chat store and canonical memory resolver; shared
  search receives the already-published definition store
  ([conversational recall](../magician/memory-recall.md)).
- HITL delivery routes (below) and bot-scoped Envoy delivery receipts beside
  chat session routes (rebuild runtime and channel bots together for
  [Envoy → Claims Review](../unified-ui/claims-review.md)).

**Mobile enrollment.** A non-loopback native listener derives a private-LAN
`http://<address>:<port>` route from the default interface each time a QR is
minted and the roster is read, so a laptop that changes networks never hands a
phone a stale address. An explicitly bound private address stays fixed.
Containers do not auto-advertise their guest address.
`MAGICIAN_MOBILE_LOCAL_ORIGIN` sets an explicit private origin or
`off`/`disabled`; the remote route comes from `mobile_access.public_origin` /
`MAGICIAN_MOBILE_PUBLIC_ORIGIN`.

## App platform wiring

**Host-read binders.** Boot registers bounded host-read binders:
`task_state`, `internal_data`, `thinking_maps_data`, `evidence_data`,
`meetings_data`, `agent_roster_data`, `tasks_data`, `notes_data`,
`memory_data`. Only the actual embedded fallback mints the built-in Apps
witness; a scoped pack claiming the name falls back to `register_override`
(parsed equality with a disk override is not provenance). See
[app-tool-bind](../magician/app-tool-bind.md).

- `meetings_data` reads threads/transcripts through the process chat store,
  published once (`chat::storage::publish_global_chat_store`) after its index
  is built; first publication wins. Reads use
  `ChatStore::list_thread_summaries_for_prefix` from the in-memory index,
  because the binder is polled every few seconds. See
  [meetings-surface.md](../magician/meetings-surface.md).
- `agent_roster_data` projects identity, display name, enabled state, the
  three `social_persona` fields and a `busy` bit, with no edit path. It takes
  the runtime's shared `Arc<AgentDefinitionStore>` so its `DefinitionCache`
  stays warm and invalidated by edits, and verifies `for_scope` actually
  re-pointed (it is a no-op on a store without a workspace layout).
- `busy` and `tasks_data` need the artifact service, which does not exist
  where boot registers the process-wide copy: that copy reports `busy` as
  `null` and fails `tasks_data` calls explicitly; per-scope registries
  (`ScopedCapabilityResolver::registry_for_scope`) attach the service.
  `notes_data` needs only the workspace layout.
- `memory_data` (`app_memory_read_v1`) shares the agent definition store and
  rereads the app's grant on every call
  ([app-memory-access](../magician/app-memory-access.md)).

**Base-catalog withholding.** `run_http_server` binds the deferred compiled
providers in place from each published pack definition in the base registry,
so `build_compiled_registry` must leave every compiled definition published.
After the last binding, boot calls
`CapabilityRegistry::withhold_unbound_compiled_packs()` on the base registry
and `remove_pack_tools` on the local tool surface, so the catalog a harness
reads via `tools/list` offers only what this process can dispatch (`read_file`,
`write_file`, `grep`, `glob` and chat-native control tools are withheld here;
scope snapshots republish them bound). `remove_pack_tools` releases its write
guard before `refresh_semantic_index` (`std::sync::RwLock` is not
re-entrant); a regression test runs it under a 5 s deadline.

**Town Square.** No `SocialWorker`: agents take turns through the
`town-square` package's `ambient_turn` behavior, owned by the app-platform
scheduler. `magician_api::social_api::SocialApi` is built from the existing
`AppPlatformApi`'s adapter, registry and scheduler handles (a second registry
would duplicate caches over one database); `/fleet-state` projects the square
through it. `SocialStoreRegistry` is still built for storage governance and
`town-square-migrate`, and must stay until the migration has run on the
deployment.

**Background behaviors.** `app_platform.background_behaviors.enabled` is the
single arming switch for host-executed schedule and event behaviors
(`spawn_background_behavior_worker`). Off when the key is absent
(`#[serde(default)]` in `magician/src/config.rs`): letting a reviewed recipe
run unattended is a deployment decision. The repo-root `magician-config.yaml`
seed sets it `true`. Startup validates that an
armed deployment has a background slot and a positive interval. Per-scope
pause, grant revocation, operation admission and resource ceilings still
apply.

**Effect admission.** App effect admission uses the reservation's absolute
deadline from durable root admission; preflight and dispatch-start persistence
neither start a separate clock nor renew it. Resource, authority, byte identity
and disclosure fences stay required. A typed admission expiry after durable
abort-before-I/O settlement permits one fresh attempt with the same arguments;
cancellation, uncertain settlement and plain error text do not.

**Registry admission.** Reads and foreground writes queue for admission on
the eight-operation pool with no registry timeout; only the caller's
cancellation or deadline ends the wait (`DEFAULT_BLOCKING_OPERATIONS` in
`magician/src/magician_v2/apps/registry.rs`). Writers acquire a blocking slot and
scope lock as a pair, never holding one while awaiting the other, and keep both
through SQLite completion including caller cancellation. Background
maintenance keeps its separate foreground reserve.

**Other startup publications.** Decision metrics starts a lazy bounded summary
writer before producers and drains it at shutdown; it opens no database or
journal on the startup thread. `execution.quality_round` defaults are
published at startup, matching reload and builder paths; admitted quality
contracts keep their pinned limits, and later allowance resets need an exact
verified owner journal action. The work-modules bridge is registered with the
host listener port: in-app calls use the authenticated owner routes under the
runtime principal/workspace, with a per-call in-memory credential revoked on
completion or cancellation. Its reviewed route table grants no submit/sweep
path; module records and reply Work use the existing scoped durable stores.

## Critical-request delivery

The composition root opens the delivery log (`system/hitl-deliveries.json`),
builds the coordinator over the user-request service, the pause store and the
process chat store (`hitl_delivery::RuntimeRequestOracle`), attaches the mobile
push dispatcher as push sink when credentials exist, starts it on the event
broadcaster and publishes it (`hitl_delivery::install_global`). Rows a previous
run left live are **re-driven**, not closed (`recover_after_restart`,
spawned): each is checked against its own request, so an alert still owed is
offered again and one whose request closed is retired. The push dispatcher's
own subscription does not handle HITL lifecycle events. Routes:
`/hitl/deliveries/{id}/claim`, `…/report`, `/hitl/deliveries`,
`/settings/critical-delivery[/test]`. See
`docs/components/magician/critical-request-delivery.md`.

## Tests

- `make test-app-reconciliation` — integration suite against production
  dependency features: reconciliation, record-policy migration, scheduled
  input admission, failed-call recovery, store query schemas and preflight
  (Town Square schema), terminal-tool mutation variants, effect-admission
  deadlines, registry admission contention. Its Schemars dev-dependency
  compiles the query-schema helper without Magician's fixture-encryption
  feature.
- `make test-app-registry-admission` — registry admission only, without
  recompiling the monolith.
- `make test-execution-harness` — execution harness seam contract test.

## Speech voice catalogs

The composition root forwards configured voice catalogs for OpenAI-compatible,
MiniMax and Gemini TTS bindings, preserving each binding's stable ID. Sarvam
and Grok retain adapter validation. The host speech status probe now carries
installed macOS voice IDs into `MacOsTtsProvider`, so `/media/providers` can
advertise host-local voices as well as FluidAudio/Kokoro and online voices.
See the [voice selection contract](../magician/realtime-media-rails.md#speaking-voice-selection).
