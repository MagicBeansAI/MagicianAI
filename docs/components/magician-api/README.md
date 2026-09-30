# Magician API

**Current development version:** `0.3.31`

API `0.3.30` supports scoped provider-health notice dismissal through the existing HITL response endpoint.

The HTTP/WS surface of the Magician service, extracted from the `magician`
monolith as a **satellite crate** over the `magician` lib. All routes below are
under `/api/magician/v2` unless stated otherwise.

## Why a satellite

`api` was the aggregation point nearly every surface hung from. Lib-module
dependencies were drained into the lib first (VibeDev project store, task lanes,
HITL metrics, observe-connector config IO, monitor support, event-scope
visibility, task ownership, the task-run factory, screen capture, scope
resolution, the today-projection cache, canonical attention, the memory API, the
MCP OAuth broker), each re-exported from its old api path, then `api/` moved
wholesale. Editing api code rebuilds only this crate, and surfaces only api
consumed (`channel_assist`, `attention_learning`, `resurfacing`, …) are
extractable behind it.

Depends on `magician` (lib), `runtime-core`, `tool-runtime-core` (exact MagicRun
pin), `magicllm`, `magician-vector-index`. Secret APIs keep Magician's
compatibility facades over `magicvault-core`. The server binary lives in
`magician-bin`. The monolith's `pub(crate)` items crossing the boundary were
widened to `pub`.

## Shape

- Media and UI preference stores resolve the workspace through
  `magician_v2::process_storage`, never `MAGICIAN_ROOT_DIR`. Durable
  object/dataset/lease I/O uses `StorageRuntime::current()`. API mining
  projection SQLite opens through `database_file_path(..., ApiMining)`.
  Durable-artifact REST and the realtime WebSocket handshake extract
  `api_scope::ResolvedScope` via `LocalPermissive`.
- Product-lane shaping lives behind seamed modules with unchanged routes:
  `media_ux::{dictation, session_controls}`, `crew_surface::{projection,
  health}`, `magician_v2/notes_projection.rs`, `magician_v2::audio_notes_seam`,
  `magician_comms::channel_assist::assist`. Attention-learning/resurfacing
  handlers import engine types from `magician::magician_v2::attention::`.
- Supervisor background loops (`magician-bg`) take their worker count from the
  pre-bootstrap `runtime.scale` plan (`current` is 4).
  `POST /settings/magician-config/reload` lists `runtime.scale` and
  `MAGICIAN_SCALE_PROFILE` as restart-required.
- HTTP handlers durably seal and schedule direct/planned work through Artifact V2
  before returning 202. Control mutations use scoped lifecycle exclusions; the
  canonical Runtime/Artifact projector owns task and execution status.

## Cross-cutting contracts

- **Startup.** The [startup contract](../magician/startup.md) exposes public,
  coarse readiness at `/startup` and `/api/magician/v2/startup`. Feature requests
  get retryable 503 until every HTTP worker has its full application. Agent
  hydration and supervisor loops wait for this barrier.
- **Auth boundary.** Ordinary API routes reject plane terminal grants.
  Login/session responses expose the canonical bearer principal. Public bootstrap
  routes discard caller scope headers.
- **Error mapping is a cross-crate contract.** Handlers translate lib error
  enums with exhaustive `match`es (`installation_review_error_response`,
  `registry_error_response`, …), so **adding a lib-side variant breaks this
  crate** and a new failure can never reach a client as an unlabelled 500. Give a
  variant its own code when it means something distinct
  (`app_installation_grant_unreadable`: the grant in force could not be read, so
  a review must not render as "nothing changed").
- **Spawn resolution.** `spawn_skill_step` and the VibeDev deploy runner override
  `PATH`, so each resolves its program with
  `runtime_core::process::resolve_program` first (bare name + PATH override makes
  Rust `fork`, which can hang in macOS atfork handlers). The skill step's
  `config/.env` applies after the PATH prepend and wins. See
  `docs/components/runtime-core/runtime-core.md`.
- **Large futures are boxed at the boundary.** An `async fn` stores its callee's
  state machine inline, so handler chains multiply stack use on 2 MiB workers.
  Boxed: `ensure_agent_startup_hydrated` (returns `Pin<Box<dyn Future>>`, awaited
  by ~41 handlers, runs at most once per process), `run_agent_startup_hydration`
  at its call site, the task-backed-root provisioning job (`with_secret_scope`,
  `ensure_dispatch_task_backed_root_inner`,
  `create_v3_task_execution_shell_boxed`), `MagicianV2Api::create_execution`
  (definition-level boxed future, pointer-size regression test), the
  agentic-resume handler, `resume_agentic_execution_with_scope_inner` (awaits
  children via `HeapAwaitExt`), and app launches (`launch_direct_app_action` and
  `execute_admitted_bridge` `Box::pin` their `invoke_direct_input`; any new
  adapter over an app launch must too). Boxing moves storage to the heap but does
  not shrink a single function's own frame — split those instead. Rule: box
  handlers that await a provisioning, hydration or dispatch chain. Test lanes do
  not raise `RUST_MIN_STACK`, so an unboxed entry point is detected. See
  [runtime async stack boundaries](../magician/runtime-async-stack-boundaries.md).
- **Cached-response headers.** Surfacing, maintenance and background-behavior
  responses are `private, no-store`.

## Agentic resume and HITL respond

- **Detached settlement.** The resume handler runs validation, the loop's job and
  settlement (next pause persisted, `Executing → WaitingUser`, outcome
  projection) as its own `tokio::spawn` task and awaits the join handle
  (`MagicianV2Api::resume_detached_from_request`). Why: Actix drops a handler's
  future on client disconnect, and settlement in the request future would leave a
  delegated child's next ask with no pause, no card and `Executing` forever. The
  task returns status, content type and bytes (`HttpResponse` is not `Send`).
- **Respond acknowledges on admission.** `POST /hitl/{id}/respond` uses
  `resume_agentic_execution_acknowledged_when_admitted`: it races the spawned
  resume against a `loop_admitted` oneshot fired once the answer passed
  validation. Earlier exits (re-ask, abort, missing pause, error) return as
  before; after admission the client gets
  `202 {accepted: true, reason: "resume_admitted"}` and later pauses arrive as
  events. The plain resume keeps its synchronous contract.
- **Missing pause.** A resume that finds no pause finalizes a possibly-orphaned
  execution (`ArtifactV2Service::finalize_orphaned_user_pause_by_execution_id`),
  after the handler drops its own lifecycle exclusion (the in-process fence is a
  non-reentrant tokio mutex). Every in-process fence wait is bounded (30 s,
  `STATELESS_LIFECYCLE_PROCESS_LOCK_WAIT`); the durable flock beneath gives up
  after 5 s.
- **Recovered pause executors** get the run's canonical event scope
  (`with_canonical_event_scope_for_resume`: registered scope, else one built from
  the pause's ids), so resumed events are persisted as runtime facts rather than
  routed transport-only, and the scope's MagicVault secret store
  (`with_scoped_secret_store_for_resume`), so an answered password stored in
  `resolved_inputs` is registered ephemerally and `[REDACTED:password]`
  placeholders resolve at dispatch (`inject_inline`).
- **Resolution timing.** The resume announces the answer (`AgenticResumed` +
  canonical `HitlResolved`) when the answer is admitted — after the fence is
  released, before the loop runs — because all pauses of a run share a
  correlation id and a later resolution would hide the next ask. A validation
  re-ask or loop failure restores the pause and re-requests it canonically
  (`emit_hitl_requested_for_restored_pause`). See
  `docs/components/magician/hitl-attention.md`.
- `magician`'s executor identity is one copy-on-write `RunIdentity`
  (`ActionExecutors::run_identity`); pause read/write uses a single snapshot so a
  resumed pause cannot name a scope neither half held. `FullPauseData.harness_pause`
  is the plane-captured action a harness approval pause waits on.
- Exact taskless Attention reads consult the private HITL SQLite authority
  across restarts and fail closed with 503 when it cannot be validated (macOS
  tests canonicalise the temp root because `/var` → `/private/var`, keeping the
  `SQLITE_OPEN_NOFOLLOW` policy real).
- Service-health notices use canonical scoped HITL; `POST /hitl/{id}/respond`
  accepts source `service_health` with choice `dismiss`.

## Routes

### Notes and memory

- `GET /notes/tree` (one level; empty `path` is root), `GET /notes/file?path=`
  (one Markdown note), `GET /notes/backlinks?path=`. `POST`/`PUT /notes/file`
  and the folder/delete routes write that folder and refresh the search index.
  The process watches each notes folder and reindexes after an outside edit is
  quiet for ~1 s. All stay inside the notes-search boundary.
- `POST /memory/search` runs `search_memory` recall for one agent in the scope
  (does not record a temperature use).
- `GET /memory/temperature/status` carries `tier_health`
  (`unearned_active_ratio`, `working_set_ratio`, `tier_lift`,
  `dead_entry_ratio`, key hygiene) from the overlay alone.
  `POST /memory/temperature/maintain` returns `compaction` when `resync_overlay`
  is set; it is the only route that may evict (candidates: `TierScope::AgentGoal`
  tiers via `discover_goal_ids_for_tier`). See [memory-evals.md](../magician/memory-evals.md).
- `POST /evidence/distill/{producer}` and `POST /screen/observations/distill`
  pass the scoped prompt manager (and screen tier) into the shared tier
  distiller; sampled background references recheck the exact cluster, tier and
  prompt before inference and export, dropping changed or deleted sources.
- Memory connections reuse feed, resurfacing and user-request endpoints; Today
  keeps individual insight IDs and detail links; the taste proposals envelope
  adds scoped `capture_health` / `capture_health_unavailable`
  ([memory connections](../magician/memory-connections.md)).
- `load_harness_supplemental_profile` resolves through
  `ArtifactV2Workspace::programs_supplemental_profile_path`, the same helper the
  learning lane writes through ([harness supplemental profile](../magician/harness-supplemental-profile.md)).

### Plane and harness engines

- `GET /plane/engines` reports `current` (process default), `chat_current`
  (`chat.harness_engine`), each engine's `native_tool_posture`, install status
  (PATH lookup, no spawns), the always-available `magician` pin, the pinned Pi
  harness when `pi` is on PATH (model list starts with `default`, leaving
  selection to Pi's isolated agent directory), and `run_pi_profile`. No auth: it
  names binaries and one config value.
- `PUT /plane/engine` persists `execution.harness_engine` + `harness_model`, and
  `pi_profile` (a chat-eligible profile validated against the router, or `null`
  for Pi's own settings) as `execution.pi_profile`. `PUT /plane/chat-engine`
  persists `chat.harness_engine` for any launchable roster name. Both pass the
  persisted engine to `set_harness_affinity(Some(engine))` unconditionally —
  `harness_affinity()` filters `""` and `magician`, so passing the built-in is
  how affinity is cleared. See [Plane](../magician/plane.md).
- Chat message POST and SSE POST accept optional `harness_engine` and
  `harness_model` beside `profile`. The engine must be installed (or
  `magician`); the route belongs to this turn, survives the pending-message
  queue, and never rewrites the server default. For Pi, `profile` resolves
  server-side to provider, model, endpoint and key variable; for other harnesses
  `harness_model` is the CLI pin (`default` lets the CLI choose). Web chat
  Settings store the choice in browser storage and send it per request; only
  `PUT /plane/chat-engine` changes the server default.
- **Plane MCP door** — `POST`/`GET`/`DELETE /plane/mcp`, loopback
  streamable-HTTP MCP. Auth accepts process-live or durable terminal `plt_`
  bearers. `initialize` advertises `tools.listChanged`, mints the session id
  `tools/call` requires, reads `params.capabilities.elicitation`, and fixes the
  session's parent engine for a terminal grant: the grant's engraved
  `harness_engine`, else the CLI family from `params.clientInfo.name`
  (`engine_family_from_client_name`), only when installed here. Every call of
  that session (and approved elicitation captures) runs with that parent engine;
  run and conversation grants keep Magician's choice. `DELETE` returns 204, ends
  the session and its terminal ledger, never the grant.
- **Durable terminal rehydration** resolves the engraved workspace's tool index
  through the shared scope catalog resolver (a cold terminal can initialize it),
  so terminals get canonical hot-tool schemas and can select deferred tools
  across requests. Missing wiring, failed resolution or an empty catalog returns
  `503 tool_catalog_unavailable` after credential validation. The grant gets its
  own runless executors narrowed to its allowlist
  (`ActionExecutors::for_runless_scope`), so scope-bound compiled tools dispatch.
  Expiry and revocation are checked every request.
- **Typed elicitation.** Capabilities bind to a freshly minted session (reusing
  an initialize ID never joins clients). Form-capable terminal calls carry
  `elicitation/create` prompts and one final result on the original POST SSE
  stream; answer POSTs return empty 202; GET notification streams are
  session-scoped. Approval requires `action: "accept"` and boolean
  `content.confirm: true`. `request_user_input` adapts text, choice,
  multi-choice and form contracts; `wait_for_run` relays questions only for runs
  this session launched. Execution pauses resume via
  `resume_agentic_execution_from_plane`, which verifies the exact displayed pause
  revision; the agent/goal/cycle selector is sent complete or not at all. The
  continuation runs on its own task, watched for 20 s so a refusal surfaces with
  its HTTP status, then polled under `timeout_secs`. Capability absence, bad
  answers, timeout, cancellation, revoked grants and cross-session answers never
  approve. See [plane typed input](../magician/plane.md#typed-input);
  `make test-plane-elicitation`.
- **Plane grants** — session-authenticated `GET/POST /plane/grants`,
  `DELETE /plane/grants/{id}`. A `plt_` grant cannot mint siblings. Mint returns
  the token once, validates the harness engine, and reports tools dropped by the
  `NEVER_ON_THE_PLANE` floor. Default concurrency 4.
- **Decision routing** — owner-authenticated `GET/PUT /plane/decision-routing`
  proxies Decision Engine model/operation settings, preserving validation
  failures and revision conflicts.

### LLM routing

Session-authenticated `GET /llm/routing`, `PUT/DELETE /llm/routing/{operation}`
(per-operation profile override), and `PUT/DELETE /llm/routing/{operation}/engine`
(`{"engine": "parent" | "pinned"}`; 400 otherwise, 404 unmapped; DELETE reverts
to the config selector).

- GET mirrors the router's staleness rule (an override naming a removed profile
  reports the default plus `stale_override`) and the flow-scoped rule:
  `affinity_scope: "flow"`, `driving_engines: {chat, run}` (`magician` is
  `null`), `engine_pins`, and per operation `engine`, `engine_source`,
  `follows_parent`, `local_floor`, `parent_profiles: {chat, run}`,
  `routing_source` (`override` | `parent` | `config`). It lists `op-harness-pi`
  as a harness profile with Pi install state. The overview is a pure function
  (`routing_overview`) of router config and `RoutingState`.
- The parent engine is the flow's, never the agent's or client's:
  `OperationRoutingOverrides` built from an agent definition carry no
  `parent_engine`, and `POST /api/magician/v3/tasks/{id}/execute`,
  `POST /api/magician/v3/monitors/{task_id}/run` and the v2 direct-execution
  routes strip it before sealing; launch admission drops it again.
- See [model-routing-panel.md](../unified-ui/model-routing-panel.md) and
  [llm-routing-overrides.md](../magician/llm-routing-overrides.md).

### Agents

`PUT /agents/{id}` (YAML) and `PATCH /agents/{id}` (merge patch) share
`update_agent_definition_with_store`, which after the trust-policy check runs
`validate_definition_llm_routing`: any newly introduced `llm_routing` profile the
router does not define or `coding_profile` not in `coding.profiles[].id` is
`422 unknown_pinned_profile` (`details.unknown`). Existing stale pins never block
unrelated edits; an unreadable config skips the check. Success writes
atomically, updates the runtime's in-memory definition (`upsert_definition`) and
invalidates the agent-list cache, so pins apply to the next run without restart.

### Components, skills and setup

- `GET /components/catalog` returns the compiled component graph, host
  description, fresh server-side observations and status (probes run beside
  Magician so remote Desktop clients get server truth). `POST /components/plan`
  takes capability ids and returns the ordered setup plan and projected report;
  unknown ids fail closed.
- Setup-token-gated: `POST /components/{id}/install` (graph component ids only;
  serialized jobs; target/script from the graph),
  `GET /components/installations/{job_id}` (bounded progress + authoritative
  post-install probe), `GET /components/admin-access` (side-effect-free token
  check), `PUT /components/{id}/configuration-file` (only for components whose
  setup descriptor declares that file primitive; catalog-owned size,
  destination and validator; response reports configured state only).
- `GET /skills/catalog` enumerates Skillshub, extra, scope-only and compiled
  skills with version, installability, setup guidance, the typed auth shape
  (kind, requirement, provider, credential names, lifecycle availability), the
  declarative setup driver from `metadata.magician.setup` (never inferred from a
  provider name), and the request's current scope. No credential values.
- Setup-token-gated `POST /skills/catalog/{name}/oauth/start` and
  `GET .../oauth/status` manage governed remote-MCP authorization with the same
  resource/issuer/profile binding and encrypted vault as MCP execution; start
  returns the authorization URL only to the native setup caller. A configured
  `mobile_access.public_origin` is the callback origin for a remote engine,
  else loopback.
- Setup-token-gated per-skill (`/skills/catalog/{name}/env`) and per-bot env
  endpoints return key names and set/unset state and accept write-only updates.
  Bot auth adds a private interactive-input endpoint (Telegram 2FA); the QR
  endpoint serves WhatsApp's PNG or renders Telegram's `tg://` login URL as SVG.
  Raw QR payloads and 2FA values are never logged or returned.
- Localhost-only `POST /secrets/app-data-root-key/rotate` requires the setup
  token plus a verified scoped identity and returns only the successor key ID.
- Secret approvals (`secret_vault_api.rs`, `SecretApprovalsResponse`) serialize
  MagicVault's `PendingSecretApproval` with `domains` beside legacy `domain`
  (a host set, or `["*"]` for all sites; omitted when empty).

### Auth and enrollment

- `POST /auth/login` (bearer session; first login on an empty install bootstraps
  the owner under `allow_signup`), `POST /auth/logout`, `GET /auth/session`,
  workspace CRUD, `GET /auth/social/{google|github}/start` →
  `GET /auth/callback/{provider}`, `POST /auth/link/{provider}/start`,
  `POST/GET /auth/tokens`, `DELETE /auth/tokens/{id}`, `POST/GET /auth/grants`,
  `DELETE /auth/grants/{id}` (session-only minting, `NEVER_ON_THE_PLANE`-floored
  allowlist, `ttl_hours` 1..=2160), `GET /auth/admin/orphaned-scopes` (behind
  `MAGICIAN_ADMIN_SECRET`), `POST /auth/identities` (owner provisions members).
- In `auth.mode: open`, a request without a bearer gets the anonymous scope
  **only while no identity exists**. Password doors are throttled (429 +
  Retry-After). Provider secrets resolve via
  `AuthProviderConfig::effective_secret(provider)` with no cross-provider
  fallback. Password login writes a credential-free audit event; the bearer gate
  records a bounded reason when `/auth/session` rejects verification.
- Bot tokens (`mag_bot_`, injected as `MAGICIAN_BEARER_TOKEN`): `GET
  /auth/session` answers with the engraved scope (`identity: null`,
  `method: "bot_token"`, `bot`, empty `workspaces`); session-only doors refuse
  with `session_required`, logout with `not_a_session`. See
  [auth.md](../magician/auth.md#bot-tokens).
- `POST /chat/enroll`, `/enroll/approve` (admin secret), `/enroll/revoke`,
  `/enroll/cancel`. `GET /chat/invoke-grammar` publishes the tutor, app_copilot,
  brainstorm and vibedev invoke grammar as one versioned, sha256-etagged catalog.

### Devices, mobile and Edge

- `GET /devices` includes `pairing_available` (an empty list does not imply the
  pairing owner is ready; no keyring diagnostics exposed) and server-owned
  `connection_options`: `same_wifi` (private-LAN origin when reachable) and
  `remote` (configured HTTPS origin, normally `https://connect.magican.ai`).
  Enrollment accepts only the matching `connection_mode`; a browser cannot submit
  an address. The origin is retained on the one-time ticket and returned by
  exchange. Pairing begin distinguishes an unavailable signing owner (503) from a
  missing/consumed exchange (404). ESP pairing requires verified local or
  Cloudflare evidence.
- Mobile connection and Android Apps enrollment payloads use only `magican://`.
  Hardware-key Apps attestation requires package `ai.magicbeans.magican`.
  - A Play build pins signer and version codes in `mobile_access`
    (`android_apps_signing_sha256`, `android_apps_version_codes`); a pinned policy
    checks the version code at enrollment.
  - A private/self-hosted build without those keys pins at owner approval: the
    chain is verified to the reviewed roots (config's, or Google's two published
    roots), with every hardware-enforced claim and the proof; the approval shows
    package, version code and signing digest, and the identity records the policy
    pinned to that build. Reconnect resolves it from the identity
    (`reviewed_policy_for_identity`), so other or later builds fail closed.
    `GET /devices/apps-automation/trust-options` reports the private build ready
    when only the address is set.
  - Refusals log content-free reasons: `[ANDROID-ATTESTATION] rejected reason=…`
    (`pins_mismatch`, `claims_mismatch`, `root_not_reviewed`, `chain_order`,
    `chain_path`, `leaf_key_curve`, `software_enforced_key`,
    `hardware_authorization`, `attestation_application_id`),
    `Apps enrollment rejected` (error class, trust mode, declared package/version,
    chain length), and `refused at stage stage=…` (`bounds`, `spki`,
    `signature`, `chain_decode`, `chain_x509`, `chain_validate`,
    `attestation_extension`, `key_description`) for `Malformed`.
- `GET /edge/bridge` — outbound desktop-capability socket. Accepts only a paired
  `desktop` credential carrying `edge_client`, binds to the roster-owned
  principal/workspace/device, and requires a versioned Edge hello as the first
  text frame. Binary, continuation, malformed, stale-generation, oversized and
  post-lease traffic closes it; pairing rotation or unpairing revokes it.
- `GET /edge/devices` lists live desktop ids and capability descriptors;
  `POST /edge/devices/{device_id}/invoke` addresses one. Paired-device
  credentials cannot call either. The server derives capability generation and
  byte ceilings from the session, mints a short-lived grant, and returns
  succeeded/failed/unavailable/cancelled. The request names execution id/epoch,
  idempotency key, capability/operation, bounded payload and a 100–120000 ms
  deadline — never a local URL or credential.

### Chat, voice and screen

- Chat SSE sends keepalive comments every 15 s while tools or human responses are
  pending. Disconnect cancels by default; `continue_on_disconnect: true` lets an
  authenticated mobile stream keep its accepted turn running for recovery. See
  [Chat SSE streaming](../magician/chat-sse-streaming.md).
- Chat and contextual-writing usage expose nullable USD costs and optional bucket
  availability, so unpriced harness turns stay unknown.
- `PUT /media/preferences` accepts `realtime_voices` (per-profile speakable
  voice), loaded by the control socket at connect.
- Backend-proxied voice injects `delegate_to_chat.chunk` text into the upstream
  Live/Gemini session so Magician's result is heard. GPT Live 1
  `session.instructions` is `voice_live_mouth_system`; the Magician-brain catalog
  snapshot is still taken so `delegate_to_chat` can authorize, but Live gets no
  Realtime tool list.
- Realtime-call accounting on the voice control socket is spawned on the
  runtime, never the actor (a socket closed right after `session.end` would
  cancel it and drop usage). `session.end` asks the provider to close and waits
  `PROVIDER_CLOSING_REPORT_GRACE`, because for a duration-billed provider the
  closing report is the whole bill. A speech start counts as cancellation only
  when assistant audio was still playing.
- Owner media registration creates a process-only, single-move credential behind
  a server-random correlation. Only direct personal-assistant voice on an
  identified owner surface with an attested backend-proxied,
  no-fallback/no-provider-storage profile receives governed app tools or
  credential-bound `local_only` context; reconnect, rotation, expiry,
  substitution, meetings, hands-free, P2P and unknown owners fail closed. On
  backend-proxied calls, user transcripts, function calls and catalog acks are
  accepted only from the server-owned provider channel. `session.ready`, first
  upstream PCM and PTT-release audio size are logged. Initial resume compaction
  is capped at 2 s.
- Gemini Live and backend-proxied GPT Realtime transcriptions are forwarded as
  `transcript.user.partial` and cumulative `transcript.assistant.delta` before
  the turn completes. Gemini 3.8 Live Extended Thinking's `interactionStatus` is
  forwarded as `interaction.status` (`in_progress` | `idle`) after
  `response.done`, because the model may still be running a non-blocking tool;
  clients map it to an `assistantWorking` flag.
- Screen capture accepts optional display-only `source_app` /
  `source_window_title` (sanitized, ≤ 200 chars). The clip toggle records via
  CuaDriver `start_recording` / `stop_recording` on one held-open
  `cua-driver mcp` session (the daemon ends a recording when its client
  disconnects) and stages the mp4 in `last_video_path`. Describe inlines the
  front window tree (`get_window_state`, `max_elements` 400) and logs driver
  `{code, suggestion}` refusals. See [screen-capture-and-ask.md](../magician/screen-capture-and-ask.md).
- `GET /contextual-writing/catalog` mirrors the desktop catalog flag-for-flag,
  including screenshot policy ([contextual-writing.md](../magician/contextual-writing.md)).

### Settings

- `GET /settings/privacy` reports effective `privacy.processing.mode` and the
  routing it produces; `PUT` dry-runs full config validation (a `mode: cloud`
  that would not load is 400) then reloads.
- `GET /settings/local-generation` reports the kitty (`qwen3.8-ud2-mtp` /
  `gemma4:12b` / `woof-4b`), host RAM, the recommendation, Ollama install status
  and RAM-tier warnings. `PUT {selected, reload_ollama?}` writes
  `selected: &local_generation_model <id>` surgically (as `make setup-ollama`
  does), reloads config and by default runs `scripts/run-ollama.sh`. Out-of-rule
  models still pin (`rule_violated`); missing models are not auto-pulled.
  Desktop onboarding treats these two endpoints as the authority for locality and
  model choice.

### Coding

`GET /coding/profiles` projects Pi, Codex, Grok, Claude Code and Antigravity.
Observation is filesystem plus cached receipt/overlay; GET never spawns a CLI.
`POST /coding/engines/{grok_acp,claude_code,agy_cli}/refresh` return 202
`checking`, coalesced 2 s. `POST /vibedev/runs/{task_id}/checkpoints/{id}/revert`
queues an engine-tagged pending resume that `run_coding_task` binds only when the
engine matches.

### Tasks, recipes and channel assist

- Recurring App task details expose schedule state and cursor-paginated history
  (25 default, ≤ 100). Recurring behaviors keep one task with per-occurrence run
  references; historical reads and cancellation select the exact root execution.
  `DELETE /apps/maintenance/action-runs/{run_ref}` removes only terminal legacy
  scheduled task shells after resource settlement. See
  [Recurring App tasks](../magician/recurring-app-tasks.md).
- Task list projections classify canonical app runs as internal; generic
  deletion is denied (results and receipts belong to the Apps lifecycle).
- Channel-assist `stats.distill.queue` and `sync/status.distill_queue` expose
  the history cutoff and disjoint eligible, history-excluded, expired and
  retry-exhausted counts; `by_state` covers retained rows. Reads never retire
  work or call a model ([Distillation](../magician/mail-assist.md#distillation)).
- Task Recipe metadata includes `version`; dashboard replay sends
  `expected_version`, and a newer compiled recipe returns 409 before any request.
  Running recipes recheck the mining switch and write authority before later
  requests, including retries. Scope mismatches are rejected. After replay, a
  statistics-save failure or mining disable keeps the result with
  `state_persistence_warning`. An uncertain write returns 409 with
  `effect_uncertain=true`, `retryable=false`, `browser_retry_allowed=false`.
  A held recipe lock returns 409 `recipe_busy` without dispatch (never queued
  behind another run's approval wait).

#### API mining and Task Recipes

Mounted under `/api/magician/v2/api-mining`, resolving the principal/workspace
into one scoped store. Payloads carry request shapes and non-secret auth status
only; captured cookies, tokens and headers stay in the encrypted SecretStore.

| Method + path | Contract |
|---|---|
| `GET /overview` | One bounded dashboard payload: recipe summaries, capabilities grouped by parent site and relevance, telemetry-hidden trace count, auth metadata, active grant count, and activity/health counters. |
| `GET /recipes` / `GET /recipes/{id}` | Recipe metadata or the complete versioned DAG; neither includes captured session values. |
| `GET /recipes/{id}/runs?limit=N` | Newest-first content-free durable run ledger. |
| `POST /recipes/{id}/replay` | Direct, non-interactive replay. An ungranted write returns 409 before any request is sent; a sent write with an unverified outcome also returns 409 and must not be retried. Generated skills add `published_only=true&expected_version=N`. |
| `GET /replay-grants` / `DELETE /replay-grants/{id}` | Audit and revoke durable request-shape grants. |
| `GET /registry?relevance=...&include_hidden=...` | Relevance-filtered learned-capability registry. |
| `GET /recipe-metrics` | Task-start lookup, replay, fallback, grant, approval, and auth-heal counters. |
| `GET /settings` / `PUT /settings` | Read or change the live scope override; only the owner can change the process ceiling. |
| `POST /settings/disable-and-purge` | Requires the exact typed confirmation, disables the scope first, then removes mining data, projection rows, grants, generated recipe catalog rows, and emitted recipe skills. |

`api_mining.enabled` is the process ceiling; a scope override can only be
stricter. While off, capture, auth drains, mining, compile/replay, generated
recipe tools and mutations are inert (mutations 409); inspection and purge stay
available. Origin purge removes every cross-origin recipe touching that origin as
one DAG (ledger, indexes, grants, generated pack, emitted skill), reports counts
separately, and is serialized against compiler publication and in-flight replays.
The optional verification worker is default-off and limited to Trusted,
read-only recipes with typed recurring-Monitor provenance. See
[API Mining Pipeline](../magician/api-mining-pipeline.md).

### Storage

- `GET /storage/activation` reports Gates 1/2/3, qualified operations and
  blocking reasons. `POST /storage/activation/cutover` requires
  `CUT OVER STORAGE` and returns 409 unless every precondition holds.
  `GET /storage` (compaction inventory) is separate.
- `GET /storage/maintenance` returns the workspace's latest Channel Assist and
  Feed maintenance state, last completion and reclaimed bytes from bounded status
  files, without opening databases ([maintenance visibility](../magician/storage-governance.md#maintenance-visibility)).
- `POST /storage/actions/app-store` accepts only typed `verify`, `optimize` or
  `reclaim` with matching confirmation, authenticates through the App transport
  scope, rejects a different query workspace, and calls the App registry owner
  (never a generic SQLite connection). Missing stores are 404 without being
  created ([App database maintenance](../magician/storage-governance.md#app-database-maintenance)).
- Attention semantic health separates active invalid/missing/dead work from
  historical queue failures ([attention worker recovery](../magician/attention-routing-funnel.md#incremental-history-maintenance-and-worker-recovery)).

### Spend and fleet

- Ceiling upsert clones under `system_ceilings.write`, then samples the ledger
  without holding ceilings. `POST /resource-authority/reservations` admits
  fail-closed ([resource-authority-api.md](../magician/resource-authority-api.md)).
- `GET /fleet-state` projects Town Square through `SocialApi::fleet_projection`
  (one reader of those rows). The section id stays `social_store` for wire
  compatibility; reason codes are `town_square_unconfigured`,
  `town_square_not_installed` (distinct: not degraded), `town_square_unreadable`.
  A failed feed read is `availability.social: unavailable`, never an empty array
  ([fleet-state-api.md](../magician/fleet-state-api.md)).

## Apps host API

### Public inventory and errors

Route registration consumes the supported-public inventory, including opaque
generation-bound logical-run cancellation and reviewed server-side action-result
composition. Inventoried routes canonicalize non-success responses to
`AppErrorEnvelope`; typed disposition owns retry and uncertainty. The signed
native owner channel stages only `snapshot`, `screenshot`, `launch`, `close`,
`tap`, `type`, `key`, `scroll`. Lifecycle transitions, grant revocation, update
abort and migration-coordinator operations return `app_installation_not_found`
404 for installations outside the scope. Unexpected governed workflow errors are
logged in full but stay opaque in responses; cancellation settles on the
structured execution runtime, off the request stack.

Terminal commits accept empty mutation batches as a receipt-free no-change
result; record-revision preconditions with an empty batch are 400.
`AppEntityMutationError::ReservedRecordId` maps to `400
invalid_app_data_mutation`.

### Entity mutations

`AppMutationOperation::Create` carries optional `record_id`, passed through by
the owner-mutation surface so a manifest can address a row by name. It is
`skip_serializing_if = "Option::is_none"`, so bodies without it keep their
canonical-JSON digest (existing idempotency keys still match). Errors:
`RecordAlreadyExists` (never an upsert), `ReservedRecordId` (`rec_` prefix is the
store's minting namespace), and a contract rejection when two operations in one
batch name the same record.

### Writing against the app data plane

Constraints that bite any caller of `owner_query` / `owner_mutate`:

- **Predicates are validated flat**: every arena node must be admissible even on
  an untakeable branch. Ordering comparisons are legal only on integer, decimal
  and timestamp — narrow on a timestamp and finish a text keyset in Rust.
- **Idempotency keys are `AppReference`s** (alphanumerics and `_-.:/@#`); hash
  user content rather than interpolating it.
- **A stable key needs a stable payload**: the replay check digests the whole
  command, so a fresh `created_at` under a fixed key is a permanent
  `IdempotencyConflict`. Make the payload deterministic or check existence first.
- **One page is 200 rows**; follow `next_cursor`.
- **Ties order by `record_id`** (opaque), so over-fetch then order when a window
  is sized exactly to a page.
- **`limit` is part of a cursor's identity** (`query_identity_digest`); keep page
  size constant and bound by row count.
- **One `In` predicate holds at most `max_collection_items` (256)**; chunk
  larger id lists.

### `/social/*` — Town Square data plane

`magician-api/src/social_api.rs` serves `/social/*` from the `town-square` app
package's entity store through the governed owner data plane. It lives here
because `authenticated_app_scope` does (one kernel, dependency one way).

- Responses are built from the original `social::types` structs (`post` is
  flattened into its reactions wrapper, `member` is not flattened into its mood
  wrapper, `next_before` is emitted as `null`).
- Writes use `owner_mutate`, not the package's `auto`-runner workflows, so a POST
  stays a synchronous 201 with a post id.
- `SocialApi` borrows `AppPlatformApi`'s `entity_adapter()`, `registry()` and
  `background_behaviors()` handles (a second registry would be a second set of
  caches).
- `/social/policy` reports `chatter_ready` and `scope_policy.{enabled,paused,state}`
  from the scope's pause control and the `ambient_turn` head; a failed health
  read reports "not configured", never healthy. `GET /social/health` remains
  (the Town Square page reads operator policy from it); `worker_global` is the
  `ambient_turn` behavior head.
- The members projection includes opted-out agents for the owner's roster;
  opt-out still gates mentions, groups and ambient selection.

### Background behaviors

- `GET /apps/background-behaviors` returns a ≤ 256-item metadata-only live health
  page; continue with all three of `before_updated_at`,
  `before_installation_id`, `before_behavior_id` (best-effort, not a snapshot).
- `PUT /apps/background-behaviors/policy` takes
  `{ "expected_revision": <u64>, "paused": <bool> }` (revision-CAS scope pause).
- Both return 503 only while `app_platform.background_behaviors.enabled` is false
  or scheduler construction failed. `worker_running` is owned by the scheduler
  (the supervisor marks it per attempt; the route returns the snapshot
  unmodified); during capped restart backoff claims pause while health and pause
  control stay available. Health holds only closed states/codes, counters, opaque
  fire refs, corruption counts and a bounded event feed — never entity values,
  prompts, provider output or raw errors.
- `POST /apps/background-behaviors/{installation_id}/{behavior_id}/retry` with
  `expected_installation_generation` and `expected_revision` requeues only a
  lease-free `workflow_launch_blocked` head; stale, running or other blocked
  states are 409. Due time, fire identity, starts, failures, budgets and grants
  are unchanged, and the worker rechecks policy, source, installation,
  permissions and capacity. `retry_scheduled` acknowledges the request, not
  success.
- App events and owner notifications have no public ingress or response route.
  A boot-installed Artifact canonical observer accepts only sealed foreground app
  root `agentic.execution_completed` facts (`succeeded|failed`) into the registry
  (V26 debt, V28 replay tombstones, V29 quarantine, V30 raw compaction cursors).
  The shared scope worker runs schedule, event and owner-notification lanes;
  owner delivery and terminal-payload maintenance run even while the
  schedule/event master is off. See the
  [threat model](../magician/app-events-owner-notifications-threat-model.md).
- Background scope sweeps admit up to four workspaces concurrently; the
  registry's fair background admission still bounds database work and preserves
  foreground headroom. Each workspace's four debt lanes (schedule, events,
  notifications, retention) keep independent budgets. Lifecycle changes make the
  next reconciliation sweep due without resetting cadence, starts, grants or
  budgets.

### Widgets, indicators and slots

- `POST /apps/widgets/render-batch` takes ≤ 12 `(installation_id, widget_id)`
  targets plus the closed native-client capability set — never a query or binder
  contract. Each target installation is read once and its snapshot reused; a
  scope epoch fences later lifecycle changes. Returns schema-v1 native models with
  per-item revisions and a batch ETag. `If-None-Match` yields an empty 304 when
  revisions are unchanged, with the ETag and the next foreground deadline in
  `X-App-Widget-Refresh-After` (CORS-exposed). The deadline is ≥ 5 s past
  `rendered_at`, and items within `max(5 s, refresh/5)` of expiry are recomputed.
  Cache keys include installation generation; the lifecycle hide hook evicts
  synchronously. Limits: 32 KiB request, 64 KiB per model, 1 MiB response.
- `GET /apps/indicators` reads ≤ 32 current, expiring materializations; it never
  evaluates an app or fans out to entity stores. Picker suggestions keep page,
  region and system-default intent with injective page-qualified slot ids.
- `GET /apps/slots/{slot_id}` is the single-slot read. `POST
  /apps/slots/resolve-batch` takes 1–12 unique page-qualified slot ids (≤ 16 KiB),
  preserves order, and returns ≤ 128 KiB from one inventory snapshot and one
  slot-state load; it requests only referenced installations
  (`AppSlotInventoryResolver::snapshot_for_installations`), releases its disk
  worker before awaiting package admission, and queues under caller cancellation.
  `GET /apps/slot-assignments` is the bounded picker page and takes the write
  fence; `POST` requires that fence, the expected revision and the exact picker
  candidate binding. Picker pages carry an inventory revision; reopen pagination
  if it changes.
- Native widget action launches may add a rendered-installation precondition
  (`generation` + `package_revision_ref`); a stale click is 409. An exact
  already-started response-lost retry recovers only control/result state; a
  sealed pending/ready retry revalidates live authority under the start permit.

### Custom surfaces v1

`/apps/installations/{id}/custom-surface-v1/host`, `.../assets/{digest}/{tail}`,
`.../bridge`, and `POST .../sessions/{session_ref}/reload-note`. All serving
routes answer the host route's 404 when `app_platform.custom_surfaces_v1.enabled`
is off. Host-open compiles a plan only for an entry point in
`granted_custom_surface_entry_points`.

- The minted `entry_url` carries the live session reference as the first segment
  after `assets/`: the one credential a sandboxed frame's opaque origin can
  present, and relative subresources inherit it by URL resolution (a query would
  be dropped by relative script references). It is a bearer credential for
  read-only serving of that installation's reviewed members; the bridge still
  needs host-page authentication and the full admitted envelope.
- `get_scripted_surface_asset_handler` resolves scope through
  `authenticated_app_scope` first and, only on the workspace-missing refusal (the
  hosted-web Cloudflare Access shape), through the live session
  (`session_bound_app_scope`, re-checking the identity's principal). Unknown or
  torn-down sessions answer `SessionGone`; wrong principals `app_scope_mismatch`.
  The bridge POST has no session fallback. See
  [custom-surfaces-v1.md](../magician/custom-surfaces-v1.md#session-bound-scope-hosted-web-cf-access-fix).
- `reload-note` is called by the host page or native shell (never the frame, whose
  CSP pins `connect-src 'none'`) for every reload and renderer crash; it reads no
  body and uses the same auth posture as assets. The kernel refuses a session of
  another installation, an expired session, and `ReloadBudgetExceeded` (3
  reloads/crashes), mapped to 409 `app_custom_surface_denied`; quarantine evicts
  the session. Any `%` in `{session_ref}` is the asset route's early 404. A leaked
  ref can at worst drive that one session to eviction.

### Staged-ingest apply

`POST /apps/installations/{installation_id}/claims-ingests/apply` is the host
half of claims-review's `stage_ingest` workflow (the package can only write an
`ingest_request` row). It reads the row's live head under one
installation/grant/schema snapshot, applies it through `TranscriptIngestion`,
and stamps `apply_state`, `actor_ref`, `act_ref` back with a revision
expectation. Body: `record_id`, `record_revision`, `effective_speaker`, `ours` /
`counterparty`, `occurred_at`, optional `consequence_class` (`private_local` and
unknown values refused). No desktop signature: recording that a room happened is
what `POST /transcripts/ingest` already allows an owner session; *settling* a
claim needs the paired desktop. Admission order: interactive owner session,
enabled installation, live source head as the final registry read, scope
liveness re-sampled at the mutation boundary. A failed stamp answers
`staged_row_stamped: false` with the act ref; apply is idempotent per
`ingest_id`. See [commitments.md](../magician/commitments.md).

### Meeting-control owner decisions

`POST /apps/installations/{installation_id}/meeting-controls/owner-decisions` is
the signed destination for the meetings console's `listen`, `join`, `pause`,
`resume`, `stop`. Admission order (as the claims-decision sibling): interactive
owner session, enabled installation, envelope and command validation,
route-authority match, package/workflow/action re-resolution, active desktop
pairing, then the live `control_request` head as the final read, with the
decision timestamp re-sampled at the mutation boundary (a start's gesture expiry
measures exactly that). `/meetings/*` routes and JSON are unchanged for their
four consumers (web, iOS `MeetingsAPI.swift`, Android, the `meeting` tool); they
append to the shared capture-control audit (refusals too), and
`/meetings/upcoming` delegates to the single calendar owner. A `wire_parity` test
pins row key sets and liveness rules. See
[meetings-surface.md](../magician/meetings-surface.md).

### Older-data cleanup

`GET /apps/installations/{id}/data-cleanup` (indexed timestamp options and the
owner's latest job), `POST .../data-cleanup/preview` (exact age selection), and
`POST .../data-cleanup/{job_ref}/control` (confirm, pause, resume, cancel) or
`/advance`. Confirm requires the preview digest and literal
`confirmation: "delete_older_app_data"`; unknown fields and cross-owner/scope
requests are rejected. Expired previews 410, changed installation/state 409, busy
final WAL checkpoint 503. Advance is idempotent after completion and commits
removal and progress atomically. Owner maintenance only, never a sandbox SDK or
bridge capability ([entity-store cleanup](../magician/app-entity-store.md#owner-approved-age-cleanup)).

### Memory access

`GET /apps/installations/{installation_id}/memory-access` returns the app's
owner-memory request, the grant made at review, the grant in force
(`interactive` / `background`, `null` = nothing), the CAS `edit_revision`,
enablement, and the user-tier catalog with `ordinary` / `sensitive`
readability. `POST` takes `{expected_edit_revision, interactive, background}`
(unknown fields refused); a stale revision or selection outside the request is
`409 app_memory_access_conflict`. Applies on the app's next memory read
([app-memory-access](../magician/app-memory-access.md)).

### Transcripts and Envoy claims

- `GET /transcripts/claims/{claim_id}/pending-confirmation` (owner-only) returns
  an admitted-but-unapplied confirmation (`decision_id`, `expected_revision`,
  `by`, `note`, `prepared_at`) or `{ "confirmation": null }`; it never applies
  it (`POST .../confirm` rechecks under the destination lock). Bot, API-token and
  grant bearers cannot read it.
- Claims Review exposes bounded status/search/cursor listings and delivery
  provenance. The Envoy delivery endpoint requires a runtime-minted bearer for
  the persisted session's bot and scope. Its request carries
  `binding: {attempt_id, channel_type, channel_address, payload_sha256}`, shared
  by all receipt phases; a missing or mismatched binding on a prepared reply is
  409. Begin retries may resume only the same unsent attempt; a fresh attempt
  gets `send: false` once dispatch began.
- Statement confirmation/rejection and commitment confirmation require an
  interactive owner session (or verified paired-device/local boundary);
  `by` is attribution only; workspace selectors must match the authenticated
  scope. See [Claims Review](../unified-ui/claims-review.md).

### `AppPlatformApi::admit_system_packages_at_boot` (2026-09-03)

Admits the deployment's `distribution: system` packages at startup, publishing
inert `ready_for_review` installations only. Which scopes it touches:
[system-package-boot-admission-scopes.md](system-package-boot-admission-scopes.md).
Failures are logged per package and never propagate (one malformed seed must not
cost the others). Trust argument:
[system-boot-admission](../magician/system-boot-admission.md).

It returns `SystemPackageBootSummary` per scope: the admission report plus
`enablement_failures`, kept apart because admission failure is bad bytes and
enablement failure is good bytes the host would not grant.
`enable_boot_admitted_package` runs the reviewer's two steps (`review`, then
`approve` presenting the shown digest). With `enable_at_boot`, admission mints a
non-transport host grantor bound to that one admitted installation; it cannot
execute an app or approve a neighbour, and ordinary system workers still cannot
approve. The schema default is off; the shipped config opts in; opted-out
deployments use `POST /apps/installations/{id}/approve`.

Boot admission refreshes the scope's widget registrations before minting system
slot defaults. The projection worker gates each drain on its own repair: a failed
personal-agent retrieval or memory-contribution repair does not skip entity,
lifecycle, capability-overlay or widget-registration drains, and repair failures
log on the first three passes then once per sixty (`app boot repair will
retry`).

## Tests

- **Fixtures must be cutover-complete deployments.** The stateless driver is the
  default and refuses scopes whose legacy-writer cutover is unsealed (503
  `stateless_scope_not_activated`, or 500 from triggers, both from
  `create_execution_inner`). Seal against the orchestrator's pause-state root:

  ```rust
  magician::magician_v2::test_support::activate_conventional_test_scopes_sync(
      &api.orchestrator().pause_states_storage_path(),
  );
  ```

  `CONVENTIONAL_TEST_SCOPES` covers the usual pairs; others use
  `activate_stateless_scope`. Seal once, in the fixture: a second seal under a
  different deployment id is refused. **Do not set
  `MAGICIAN_EXECUTION_DRIVER=inprocess`** to pass tests — it hides the gap by not
  exercising the production driver. Steers are driver-specific (`Inprocess`
  pushes to `steer_queue`; `Stateless` writes a durable inbox), so tests
  asserting on the queue gate on `ExecutionDriver::from_env()`.
- **`apps_api.rs` synchronous tests** must use `#[::core::prelude::v1::test]`:
  its `use actix_web::{.., test, ..}` imports actix's `#[test]` macro, which
  requires an `async fn`.
- Boot enablement coverage:
  `configured_boot_enablement_surfaces_every_system_package_and_pins_defaults`,
  `an_owner_can_enable_every_boot_admitted_system_package`,
  `admission_without_enablement_leaves_every_package_inert`.
- `make test-plane-elicitation` — plane door/adapter contracts and the official
  SDK client regression, without the monolith fixture feature.
