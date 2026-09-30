# Magician Bin

**Current development version:** `0.2.20`

This development version packages Magician `0.7.89` and API `0.3.31`, including [shared-decision reliability fixes and provider-health HITL notices](../magician/structured-decision.md).

The `magician` server binary and composition root, in its own package. A
satellite `magician-api` depending on the `magician` lib cannot live in the lib's
package (`magician → magician-api → magician`), so `magician-bin` depends on
both and nothing depends back on it. The binary name stays `magician`, so
`replace-release-bin.sh` and `make build-all-release` are unaffected.

## Shape

- `src/main.rs` — server wiring (actix bootstrap, service graph, CLI
  subcommands).
- `src/adapters/` — local tool services.
- Depends on `magician`, `magician-api`, `runtime-core`, `tool-runtime-core`
  (through the root's exact MagicRun pin), `magicllm`, and CLI/HTTP deps;
  `once_cell` holds the process-lifetime cells installed at boot. Shared custody
  goes through Magician's MagicVault facades; bootstrap, keychain identities and
  runtime configuration stay product-owned.
- `magician-core` is a dev-dependency with `test-support`, because the execution
  harness seam's `turn_engine.rs` renders through the prompt store and a test
  binary never installs the global PromptManager
  (`make test-execution-harness`; see
  [execution harness conformance](../magician/execution-harness-conformance.md)).

## Boot sequence

1. **Runtime env files** — `$MAGICIAN_ROOT_DIR/.env.development` then `.env` load
   once in `main`, right after argument parsing and before config load, router
   construction or any credential read, on every path including the server.
   `dotenvy` never overwrites an existing variable, so precedence is real
   environment > `.env.development` > `.env`. Why: `magic-supervisor` and
   `scripts/run-supervisor.sh` source nothing, so a LaunchAgent/GUI launch would
   otherwise miss keys. The loader is `Once`-guarded.
2. Provider-free app authoring returns before config load.
3. **Storage bootstrap** — `--storage-bootstrap` /
   `MAGICIAN_STORAGE_BOOTSTRAP_CONFIG` are resolved by `magician-storage` before
   `magician-config.yaml` is opened; neither keeps the local `workspace_storage`
   profile. Live paths then load config and resolve `runtime.scale`
   (`MAGICIAN_SCALE_PROFILE` overrides the name) to build the main / execution /
   Lance Tokio runtimes.
4. After the workspace root resolves, boot installs one `StorageRuntime` under
   `.magician-storage/` **before** CLI / `--reindex` early returns. The exclusive
   default `anonymous`/`default` scope lease is taken only on the server path.
   Remote profiles fail closed at default startup; health is
   `GET /health/storage`. Satellites use the installed runtime, never
   `MAGICIAN_ROOT_DIR`; host SQLite opens through `host_database_path`. See
   [magician-storage](../magician-storage/README.md). Boot also installs
   `DeviceTransport::LocalLoopback` with the configured Magicutor CDP and Ollama
   embedding URLs.
5. **Jail cleanup (server path)** — removes this user's leftover
   `magician-app-credentials-<pid>-<uuid>` directories whose process is dead or
   not this one, then runs MagicRun's `sweep_stale_jail_members()` once to kill
   processes behind member sentinels left by a process that died mid macOS
   exec-roots jail. Counts are logged (`[APP-JAIL]`); an unreadable sentinel is
   kept and warned.
6. **LLM** — canonical LLM journal recovery runs on the blocking lane alongside
   App package admission; boot joins it before content capture, dispatch
   attachment or HTTP readiness, and recovery errors still stop startup.
   Built-in chunk adapters register before the live operation router is built.
   When `llm.dispatch.enabled`, one `LlmDispatchQueue` is installed on both
   `OperationLlmRouter` and `MultiLLMService`, so operation calls and chat share
   provider caps, RPM/TPM and `global_cloud_concurrency` (with `false` the queue
   is built but both callers route directly). The queue uses
   `OperationLlmRouter::live_dispatch_router()`, so a config reload changes its
   dispatch without restart. Provider registration skips an uninstantiable
   profile with a warning and fails only when *no* profile registered
   (`magicllm::bootstrap`).
7. **Coding-engine workers** — `spawn_codex_qualify_worker`,
   `spawn_grok_version_worker`, `spawn_claude_version_worker`,
   `spawn_agy_version_worker`, so HTTP never waits on `codex app-server` or
   `--version`. Grok overlays CLI version, filesystem/env auth and ACP initialize
   isolation (omitted MCP/tool lists stay unqualified). Claude overlays version,
   Max/OAuth and init-only isolation. Agy overlays version, Antigravity OAuth and
   init-only isolation (`init.tools` + `permission_mode`; identity is path +
   mtime + length).
8. **Attention training worker** runs when actionability, routing or bandit
   `training.enabled` is true. Shipped bandit YAML is `shadow` with no snapshot
   pin; Magician auto-installs a store snapshot and may promote it to canary.
9. **Auth** — boot opens `AuthStore` under the runtime root, attaches it as
   `AuthRuntime`, and wraps `/api/magician/v2` and `/v3` with
   `auth::middleware::authenticate_request` as the outermost gate (before
   Cloudflare Access; inert in `auth.mode: open` without a bearer; see
   [auth](../magician/auth.md)). Its 401 carries CORS headers.
   `magician_v2::cors::api_cors_middleware` wraps innermost on both scopes,
   granting the wildcard and answering every `OPTIONS` before routing, because a
   nested `web::scope` never falls back to a `/{tail:.*}` catch-all and would 404
   the preflight.
10. **Harness affinity** — last-driver-wins: the run engine is primary; when it
    is `magician`, the chat mouth's engine drives. The resolved name is passed to
    `set_harness_affinity` as `Some(engine)` unconditionally (the getter filters
    the built-in, so this both sets and clears). Boot installs
    `install_chat_harness_snapshot` from `chat.harness_engine` (default
    `magician`) beside `execution.harness_engine`, and loads
    `install_llm_routing_overrides`.
11. **System-package admission** — startup awaits
    `AppPlatformApi::admit_system_packages_at_boot` (`distribution: system`:
    `meetings, town_square, claims_review, learning, thinking_map`) before the
    app projection worker or any route can look for them. Awaited, not spawned,
    so nothing answers and caches "not installed" for a package seconds from
    existing. Publication is idempotent by content. Staging and publication
    refuse a system manifest without this owner. See
    [system-boot-admission](../magician/system-boot-admission.md).
12. **Composition latch** — agent startup hydration spawns immediately after
    `stateless_loop_lifecycle_ready.cancel()`, beside the other lifecycle
    owners. Why: hydration re-arms autonomous goals, and a due goal dispatching
    before `AgentRuntime`'s `artifact_v2_service` is wired burns the cycle
    (`record_failed_goal_cycle`) instead of deferring it. The ordering test
    asserts `opened < hydration`.
13. Startup recovery: the pause reaper removes records whose execution directory
    is gone, whose `state.json` never landed, or whose durable state is terminal
    (`pause_execution_record_is_gone_or_terminal`); a real I/O failure keeps the
    record. The synthesis reconcile sweep budgets each task itself (no outer
    timeout) and is idempotent.

## Execution driver and shutdown

Stateless is the default; `MAGICIAN_EXECUTION_DRIVER=inprocess` is an explicit
rollback. `GET /health/execution-driver` reports the process-resolved driver
(`ExecutionDriver::select`), any unrecognised value, and per-arm run counts
since start (after restart, `resolved_driver` should be `stateless` and
`runs_since_start.stateless` should advance). Shutdown keeps the wake supervisor
until claimed child handlers finish generation-bound handoff, then closes the
execution-job registry, refuses late launches, drains in-memory futures, and
only then tears down event, queue and storage sinks. Cancellation at this
boundary never fabricates a terminal execution.

## CLI subcommands

- `seal-stateless-loop-cutover --confirm-legacy-writers-drained` — immutable
  per-scope seal after drain
  (cutover runbook).
- `magician storage inventory|status|plan|export|import|verify|checkpoint|resume|cancel|cutover|rollback`
  — explicit operator workflow; cutover fails closed until every precondition
  holds ([task-19-activation.md](../magician-storage/task-19-activation.md)).
- `magician ollama-launch-config` — answers under `privacy.processing.mode:
  cloud` with `generation_model_count=0` and sizes the daemon context from the
  embedder, so `run-ollama.sh` never falls back to a locality-unaware resolver.
- `magician town-square-migrate` — one-shot migration of the retired
  first-party Town Square corpus into the `town_square` package's entity store.
  A CLI rather than a route: operator-run once, no auth surface, not
  network-reachable. It refuses when there is no enabled `app:town-square`
  installation and exits non-zero when the migration is not faithful (the exit
  status is the go/no-go for retiring the source). `--history-only` restores
  historical conversation entities into a used square, preserving live roster,
  moods, policy and cursor; no source data is deleted.
- The live `magician app` client covers candidate publication, reviewed
  lifecycle and re-enable, update/reinstall migration and rollback, purge
  recovery, and package/data/combined portability. It reads archive passphrases
  only from bounded no-follow files and never opens registry or staged-generation
  storage directly.

## Routes mounted here

Handlers often live in `magician-api`.

- `GET/PUT /plane/decision-routing` — owner-only Decision Engine routing.
- `POST /memory/search` — the owner Memory page, same recall as `search_memory`.
- `GET /components/catalog`, `POST /components/plan`, and setup-token-gated
  component installation job routes (probes, capability planning, serialized
  execution, post-install verification).
- `GET /edge/bridge` (paired Desktop Edge outbound WebSocket; hello/lease/
  generation protocol, server-side capability registry), `GET /edge/devices`,
  `POST /edge/devices/{device_id}/invoke` (typed dispatch with a server-minted
  grant).
- `POST/GET/DELETE /plane/mcp` — loopback MCP; bearer is a `plt_` grant;
  `DELETE` ends a streamable-HTTP session, never the grant; `tools/call` is
  governed dispatch when the grant has live executors ([the plane](../magician/plane.md)).
- `GET/POST /plane/grants`, `DELETE /plane/grants/{id}`; `GET /plane/engines`
  (roster, install status, `native_tool_posture`); `PUT /plane/engine`;
  `PUT /plane/chat-engine` (any launchable roster name).
- `GET/PUT /settings/privacy` (`privacy.processing.mode`, applied by reload);
  `GET/PUT /settings/local-generation` (pins
  `runtime.ollama.local_generation.selected`; RAM-tier warnings, then reload +
  `run-ollama.sh`).
- `GET /chat/invoke-grammar` (additive-only lane invoke grammar).
- `GET /llm/routing`, `PUT/DELETE /llm/routing/{operation}`,
  `PUT/DELETE /llm/routing/{operation}/engine`.
- `POST /contextual-writing/actions`, `POST /contextual-writing/sessions`,
  `GET /contextual-writing/catalog` ([contextual-writing](../magician/contextual-writing.md)).
- `POST /coding/engines/{codex_app_server,grok_acp,claude_code,agy_cli}/refresh`
  (202).
- `GET /media/providers` advertises each selectable realtime profile's voice
  catalog. Boot registers Gemini 3.5 Transcribe (`gemini_transcribe`) for
  Dictation file STT and Gemini 3.5 Live Transcribe (`gemini_live_transcribe`)
  for Hands-free streaming STT when `GEMINI_API_KEY` is present.
- HITL delivery routes (see below) and bot-scoped Envoy delivery receipts beside
  chat session routes (rebuild runtime and channel bots together for
  [Envoy → Claims Review](../unified-ui/claims-review.md)).

Mobile enrollment: a non-loopback native listener derives a private-LAN
`http://<address>:<port>` route from the default interface. Containers do not
auto-advertise their guest address. `MAGICIAN_MOBILE_LOCAL_ORIGIN` sets an
explicit private origin or `off`/`disabled`; the remote route comes from
`mobile_access.public_origin` / `MAGICIAN_MOBILE_PUBLIC_ORIGIN`.

## App platform wiring

**Host-read binders.** Boot registers bounded host-read binders — `task_state`,
`internal_data`, `thinking_maps_data`, `evidence_data`, `meetings_data`,
`agent_roster_data`, `tasks_data`, `notes_data`, `memory_data`. Provenance rule:
only the actual embedded fallback mints the built-in Apps witness; a scoped pack
claiming the name falls back to `register_override` (parsed equality with a disk
override is not provenance). See [app-tool-bind](../magician/app-tool-bind.md).

- `meetings_data` reads threads/transcripts through the process chat store,
  published once (`chat::storage::publish_global_chat_store`) right after its
  index is built. The slot is read-only and first publication wins. Reads use
  `ChatStore::list_thread_summaries_for_prefix`, answered from the in-memory
  index, because the binder is polled every few seconds. See
  [meetings-surface.md](../magician/meetings-surface.md).
- `agent_roster_data` projects identity, display name, enabled state, the three
  `social_persona` fields and a `busy` bit, with no edit path. It takes the
  runtime's shared `Arc<AgentDefinitionStore>` so its `DefinitionCache` stays
  warm and invalidated by edits, and verifies `for_scope` actually re-pointed
  (it is a no-op on a store without a workspace layout; "silently unscoped" must
  never ship).
- `busy` (roster) and `tasks_data` need the artifact service, which does not
  exist where boot registers the process-wide copy: that copy reports `busy` as
  `null` and fails `tasks_data` calls explicitly; the per-scope registries
  (`ScopedCapabilityResolver::registry_for_scope`) attach the service.
  `notes_data` needs only the workspace layout.
- `memory_data` (`app_memory_read_v1`) shares the agent definition store and
  re-reads the app's grant on every call ([app-memory-access](../magician/app-memory-access.md)).

**Base-catalog withholding.** `run_http_server` binds those deferred compiled
providers in place by reading each published pack definition from the base
registry, so `build_compiled_registry` must leave every compiled definition
published. After the last in-place binding, boot calls
`CapabilityRegistry::withhold_unbound_compiled_packs()` on the base registry and
`remove_pack_tools` on the local tool surface, so the control-plane catalog a
harness reads via `tools/list` offers only what this process can dispatch
(`read_file`, `write_file`, `grep`, `glob` and chat-native control tools are
withheld here; scope snapshots republish them bound). `remove_pack_tools`
releases its write guard before `refresh_semantic_index` (`std::sync::RwLock`
is not re-entrant); a regression test runs it under a 5 s deadline.

**Town Square.** No `SocialWorker`: agents take turns through the `town-square`
package's `ambient_turn` behavior, owned by the app-platform scheduler.
`magician_api::social_api::SocialApi` is built from the existing
`AppPlatformApi`'s adapter, registry and scheduler handles (a second registry
would duplicate caches over one database); `/fleet-state` projects the square
through it. `SocialStoreRegistry` is still built for two readers — storage
governance and `town-square-migrate` — and must stay until the migration has run
on the deployment.

**Background behaviors.** `app_platform.background_behaviors.enabled` is the
single arming switch for host-executed schedule and event behaviors
(`spawn_background_behavior_worker`). Default **off**: letting a reviewed recipe
run unattended is a deployment decision. Startup validates that an armed
deployment has a background slot and a positive interval. Per-scope pause, grant
revocation, operation admission and resource ceilings still apply.

**Effect admission.** App effect admission uses the resource reservation's
absolute deadline from durable root admission; preflight and dispatch-start
persistence neither start a separate clock nor renew it. Resource, authority,
byte identity and disclosure fences stay required. A typed admission expiry
after durable abort-before-I/O settlement permits one fresh governed attempt
with the same arguments; cancellation, uncertain settlement and plain error text
do not.

**Registry admission.** Reads and foreground writes wait up to 5 s for admission
on the eight-operation pool. Writer admission acquires a blocking slot and scope
lock as a pair, never holding one while awaiting the other, and keeps both
through SQLite completion including caller cancellation. Background maintenance
keeps its separate foreground reserve.

## Critical-request delivery (secure HITL P5)

The composition root opens the delivery log (`system/hitl-deliveries.json`),
builds the coordinator over the user-request service, the pause store and the
process chat store (`hitl_delivery::RuntimeRequestOracle`), attaches the mobile
push dispatcher as push sink when credentials exist, starts it on the event
broadcaster and publishes it (`hitl_delivery::install_global`). Rows a previous
run left live are **re-driven**, not closed (`recover_after_restart`, spawned):
each is checked against its own request, so an alert still owed is offered again
and one whose request closed is retired. The push dispatcher's own subscription
does not handle HITL lifecycle events. Routes: `/hitl/deliveries/{id}/claim`,
`…/report`, `/hitl/deliveries`, `/settings/critical-delivery[/test]`. See
`docs/components/magician/critical-request-delivery.md`.

## Tests

- `make test-app-reconciliation` — focused integration suite against production
  dependency features: reconciliation, record-policy migration, scheduled input
  admission, failed-call recovery, canonical store query schemas and preflight
  (Town Square schema), terminal-tool mutation variants, effect-admission
  deadlines, registry admission contention. Its Schemars dev-dependency compiles
  the runtime's query-schema helper without Magician's fixture-encryption
  feature.
- `make test-app-registry-admission` — registry admission only, without
  recompiling the monolith.
- `make test-execution-harness` — execution harness seam contract test.
