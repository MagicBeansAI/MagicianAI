# Magician V2 API Guide

Service: the `magician` binary (`magician-bin`); routes in `magician-bin/src/main.rs`, handlers in `magician-api`. Binds **http://127.0.0.1:3002** (`--host` / `--port`; all interfaces only with `--host 0.0.0.0`).

Two scopes share one bearer middleware: **V2** `http://127.0.0.1:3002/api/magician/v2` (executions, chat, meetings, agents, bots, skills, HITL, settings, API mining) and **V3** `http://127.0.0.1:3002/api/magician/v3` (tasks, plans, monitors, NDJSON event tail). Surfaces with their own docs (apps, feed, learning, media, plane, devices, auth internals) are linked. Extra JSON keys are not contract. Realtime: `GET /api/magician/v2/realtime/ws` ([V2 WebSocket Events](v2-websocket-events.md)). Auth: [auth.md](auth.md).

## Auth

Except public doors (`POST /auth/login`, social start/callback, `POST /chat/enroll/approve`, one-time device enrollment exchange, ESP `POST /devices/pair`, `/plane/mcp`), send `Authorization: Bearer <token>`, minted for one principal/workspace. `X-Principal`, `X-Workspace` and `principal`/`workspace` query/body selectors are not authority. Browser WebSockets offer a protocol (`magician-events-v2` realtime, `magician-voice-control-v1` voice control) plus `magician-bearer.<token>`; middleware consumes the bearer; the server selects only the protocol and never echoes the credential.

In `auth.mode: open` with an empty identity store, a missing bearer is `anonymous`/`default`; once an identity exists, bearerless requests are refused.

## Data model

Wire types: `magician/src/magician_v2/storage/models.rs`.

### ExecutionRun

`GET /executions/{id}` returns this (no turns/slots); create returns `{ execution_id, execution, initial_message_enqueued, skip_planning }` (plus `task_id`, `plan`, `planning_execution_id` when planned).

| Field | Notes |
| --- | --- |
| `id`, `principal`, `workspace` | Stored scope; request scope is bearer-derived. |
| `task_id`, `root_execution_id`, `parent_execution_id`, `child_execution_ids` | Task-backed tree. |
| `title` | Optional. |
| `waiting_state` | `Planning`, `PlanningComplete`, `Runnable`, `Executing`, `Sleeping` (durable timer, not a user pause), `WaitingChildren`, `WaitingUser`, `Completed`, `Failed`, `Cancelled`, `Paused`. |
| `created_at`, `updated_at` | Unix ms. |
| `processing_correlation_id` | In-flight orchestration. |
| `current_stage`, `current_provider` | Optional telemetry. |
| `escalation_trigger` | Paused by `CannotProceed` / `LoopDetected`. |
| `active_owner_agent_id`, `owner_stack`, `active_delegation_group`, `delegation_chain` | Ownership / handover. |
| `work_authority` | Inherited work context; absent = unbound, not unrestricted. |
| `paused_from_state` | Exact pre-pause `WaitingState`. |
| `entry_mode` | `planning_backed` or `direct`. |

Clarification session/history live on `ExecutionRunDocument`. No `/slots` route; slots are a document field (`pending_slots_count` on summaries).

### V2Turn

`id`, `execution_id`, `direction` (`Inbound` / `Outbound`), `text`, `created_at`; optional `in_reply_to_slot_id`, `recommended_questions`, and `enriched_query` (a **string**, the rewritten query); planner metadata `query_analysis`, `analysis_metadata`, `strategy_attempts`, `processing_metadata`.

### Execution list row

`GET /executions` → `PaginatedResult<ExecutionSummary>`: `items` + `pagination.{total,limit,offset,has_more}`; rows add `turn_count`, `pending_slots_count`, owner/delegation fields, `escalation_trigger`. `limit` default 50, max 200; `updated_at` descending.

## Health and mode

Outside the `/api/magician/v2` auth wrap:

| Route | Description |
| --- | --- |
| `GET /health` | Liveness plus best-effort Magicutor / desktop probes. |
| `GET /health/storage` | Storage profile, adapter health, scope-lease generations; `503` on a lost lease. |
| `GET /health/execution-driver` | Resolved `MAGICIAN_EXECUTION_DRIVER`, unrecognised-value flag, run counts since start. |

`GET /api/magician/mode` (also outside v2) → `{ "mode": "standard", "timestamp": "<rfc3339>" }`.

## Executions

Prefix `/api/magician/v2`. Control routes (`cancel`, `abort`, `pause`, `resume`, `steer`, the delete alias) need a workspace-bound bearer; out-of-scope executions are not found.

| Route | Description |
| --- | --- |
| `POST /executions` | Task-backed shell. Optional `title`, `initial_message`, `ui_thread_id`, `skip_planning`, `max_iterations`, `llm_routing_overrides`, `env_mode`, `debug`, `internal`, `engagement_id`. `internal: true` → Internal task without debug/`__system__` provenance; `debug: true` adds it. Scope from the bearer. `202` (launch accepted durably) with `initial_message`, else `200`. `initial_message` + `skip_planning: false` is V3 task planning, not the legacy pipeline: creates and plans an approved V3 task (as `POST /v3/tasks/{id}/plan`): `execution_id` is the planning id (`planexec_…`), `execution` is `null` until created, plus `task_id`, `plan` (`TaskPlanRecord`), `planning_execution_id`. With `engagement_id`: `400 engagement_planning_unsupported`. |
| `GET /executions` | Paginated (`limit`, `offset`; `workspace` ignored). |
| `GET /executions/{id}` | One `ExecutionRun`. |
| `DELETE /executions/{id}` | Cancel tree, delete; `204`. |
| `GET /executions/{id}/status` | `{ execution_id, waiting_state }`. |
| `PUT /executions/{id}/status` | `{ "status": "Paused" }` or another `WaitingState`. |
| `GET /executions/{id}/control-state` | Authoritative `can_pause`, `can_resume`, `can_steer`, `can_cancel`; fails closed mid-mutation. |
| `POST /executions/{id}/start` | Direct agentic start on an existing run: `{ "goal": "...", "max_iterations"?, "llm_routing_overrides"? }`. |
| `POST /executions/{id}/cancel` | Cancel in-flight orchestration. |
| `POST /executions/{id}/abort` | Alias for `/cancel`. |
| `DELETE /executions/{id}/execution` | Same cancel handler; stops a live agent, keeps the record. |
| `POST /executions/{id}/pause` | `pause_execution_tree`; succeeds only when every running node has a durable exact-resume checkpoint and is `Paused`. `{ paused: true }`; invalid when not live. |
| `POST /executions/{id}/resume` | `resume_execution_tree`; validates tree and every required checkpoint before consuming one. `{ resumed: true }`; invalid when not resumable. |
| `POST /executions/{id}/steer` | `{ "message": "..." }`, FIFO into the next decision turn as additive `[OPERATOR STEER]` guidance. 4 KiB, 16 pending, full `409`; a run that is not executing (queued, planning, waiting, paused) is also `409` with its `waiting_state`. `{ steered: true }`. |
| `POST /executions/{id}/execution/agentic-continue` | After a max-iterations (or similar) pause. Optional `pause_state_id`, plan/step/agent/goal/cycle keys, `guidance`. Not HITL. |
| `POST /executions/{id}/execution/agentic-cancel` | Cancel a paused agentic run. |
| `GET /executions/{id}/execution-summary` | Latest persisted agentic summary `{ execution_id, summary }`. |
| `GET /executions/{id}/responsibility` | Owner chain, handover, blocking children, `responsibility_summary`. |
| `GET /executions/{id}/execution-panel` | Panel projection (also V3). |
| `GET /executions/{id}/pause-state` | Debug pause-store snapshot. |
| `GET /executions/{id}/observations` | Observations. |
| `GET /executions/{execution_id}/observations/{observation_id}/screenshot` | Screenshot. |
| `GET /executions/{execution_id}/observations/{observation_id}/json` | Raw JSON. |
| `GET /executions/{execution_id}/storage-stats` | Storage stats. |

Pause, resume and cancel serialize on the root execution id; status transitions share per-execution locks with them. Manual-pause checkpoints must be durable before `Paused`; persistence failure fails closed.

`POST /api/magician/v2/admin/tasks/reconcile-orphaned-ready` repairs agent-created tasks orphaned in `ready`: `dry_run: true` default, `batch_cap` 1–100 (default 25), scope from request headers (optional body `workspace`); applying returns 409 `auto_dispatch_disabled` while auto-dispatch is off. CLI: `magician reconcile-orphaned-tasks [--api-base <url>] [--batch-cap <n>] [--apply]` (bearer from `MAGICIAN_BEARER_TOKEN`).

One harness (the direct agentic loop); a stored `PlanGraph` is advisory; no strategy selector.

### Turns

| Route | Description |
| --- | --- |
| `GET /executions/{id}/turns` | `direction=Inbound\|Outbound`, `limit`, `offset`. |
| `POST /executions/{id}/message` | Agentic run with `text` as goal: `{ "text": "...", "skip_planning": true, "max_iterations"?, "llm_routing_overrides"? }` → `202 { execution_id, skip_planning: true }`. Legacy V2 planning (`skip_planning` false or omitted) is retired: `400 { error: "legacy_planning_retired", message, execution_id }`; plan via `POST /executions` + `initial_message` or `POST /api/magician/v3/tasks/{id}/plan`. `in_reply_to_slot_id` / `force_new_workflow` ignored. |

No `/executions/{id}/analysis` or `/analysis/{turn_id}`; analysis is on V3 (`GET /api/magician/v3/tasks/{task_id}/plan/analysis`, `POST /api/magician/v3/tasks/{task_id}/analyze`; standalone `/analyze` is gone).

## Chat and consumer channels

Shared backend for the web client and channel bots: [Consumer Channels](../../consumer_channels.md), [chat-mode.md](chat-mode.md).

| Route | Description |
| --- | --- |
| `POST /chat/enroll` | Enroll or re-enroll: `{ "channel_type", "channel_address", "display_name"? }` in the bearer workspace → `{ enrolled: true, principal }` (auto-approved) or `{ enrolled: false, code }` (approval required). |
| `POST /chat/enroll/approve` | Public admin door; bearer is `MAGICIAN_ADMIN_SECRET`, not a `mag_` token. `{ "code", "principal", "workspace"? }` (default `default`). |
| `POST /chat/enroll/revoke` | `{ "channel_type", "channel_address" }`; holder revokes own, owner any. |
| `POST /chat/enroll/cancel` | By one-time `code`, or owner by channel identity. |
| `GET /chat/enroll/status` | `channel_type` + `channel_address`. |
| `GET /chat/active` | Get/create active session; optional `channel` + `channel_address`. |
| `POST /chat/new` | Start a chat on the thread: returns the thread's newest untitled, empty personal chat for the same agent and channel when one exists (user threads; not feature/rotating threads or forced agents), else creates one. |
| `GET /chat/sessions` | List. |
| `GET /chat/sessions/{id}` | Session + history + `turn_state` (latest turn's status; [reconnect contract](chat-sse-streaming.md#turn-state-on-reconnect)). |
| `PATCH /chat/sessions/{id}` | Metadata. |
| `GET/POST /chat/sessions/{id}/messages` | List / send (→ `ChatResponse`). |
| `POST /chat/sessions/{id}/transcript` | Display-only line (`system`); never dispatches; archived `400`. |
| `DELETE /chat/sessions/{id}/run` | Cancel an in-flight turn. |

`ui_thread_id` on `GET /chat/active`, `POST /chat/new` and `GET /chat/sessions` may be a thread's slug or a former slug (see UI threads below): the server maps a key that names a live thread to its canonical id, so sessions are always filed under the id. A key that names no live thread passes through unchanged (a first reference still creates that thread id).

## UI threads

Chat threads (`/t/<key>` on the web). A thread has a permanent `id` (sessions, tasks and memory reference it; it never changes) and a `slug`, the URL key that follows its name. Records (`UiThreadRecord`) carry both: `{ principal, workspace, id, slug, name, archived, sort_order, memory_summary, memory_updated_at, created_at, updated_at, history_lane, display_mode, plan_mode }`. A client that sees no `slug` (older server) uses `id`.

**Key resolution.** `{key}` below is an id, the current slug, or a former slug (alias), tried in that order against live threads; the response carries the canonical `id` and current `slug`. Clients should keep the `id` for every later call and build links from the `slug`.

**Slug rules.** Unique per scope: never another thread's id (deleted ones included), live slug or alias. A collision appends `-2`, `-3`…. A name that slugs to nothing falls back to the id. `new` is reserved (the web "name a new thread" route): no thread is ever created with id or slug `new`. `general` keeps slug `general` through any rename.

| Route | Description |
| --- | --- |
| `GET /ui-threads` | List (`threads`, `total`, `limit`, `offset`); `history_lane`, `archived=exclude\|include\|only`, `q`, `limit`, `offset` page it. |
| `POST /ui-threads` | Create. `{ "name": "...", "id"?: "..." }`. Without `id` (or blank) the server derives a fresh unique id from the name, slug = id, and never renames an existing thread — the path for "New thread". An explicit `id` keeps create-or-rename semantics; `id: "new"` is treated as no id. → `UiThreadRecord`. |
| `GET /ui-threads/{key}` | `{ thread: UiThreadDetail }` (record + `memory_text`); `404` when the key names no live thread. |
| `PATCH /ui-threads/{key}` | `{ name?, archived?, memory_text?, display_mode?: "chat"\|"dev", plan_mode? }` → `{ thread }`. A new `name` moves the slug to the unique slug of that name and keeps the previous slug as an alias, so old links keep resolving; the id never changes. A key that names no stored thread is taken as an id, so `archived` still archives or restores the sessions of a thread that exists only through them. |
| `DELETE /ui-threads/{key}` | `{ deleted: true, id }` with the canonical id; `#general` cannot be deleted. A deleted thread's id, slug and aliases stop resolving. |
| `POST /ui-threads/reorder` | `{ ordered_ids: [id, …] }` (ids, not slugs) → list. |

Storage: `ui_threads.duckdb` holds `ui_threads.slug` and `ui_thread_slug_aliases (principal, workspace, slug → thread_id)`. Opening a scope backfills a missing slug from the id, except a row whose id is reserved (`new`, made by the old web path), which gets a unique slug from its name.

## Meetings

A passive listen and an agent join of one meeting share a dated `meeting-*` thread. Bearer scope. Rails and WhatsApp: [meetings.md](meetings.md).

| Route | Description |
| --- | --- |
| `GET /meetings` | Active sessions (both rails) + recent `meeting-*` threads (`ui_thread_id` prefix). |
| `GET /meetings/active` | Live only; ended sessions stay readable on `/meetings/{id}` for 5 minutes. |
| `GET /meetings/upcoming` | Next ~12h across GWS accounts, queried concurrently, merged, deduped (meet link / title+start), by start. Accounts: `MEET_BOT_CALENDAR_ACCOUNTS` → legacy `MEET_BOT_CALENDAR_ACCOUNT` → `gws_accounts` in runtime operator-config `runtime_config_path("operator-config.yaml", "skillshub/operator-config.yaml")` (only profiles existing under `<scope>/auth/gws-<name>`, the store the `calendar` skill and GWS bots use) → `work`. Binary `GWS_BINARY` → skillshub-local → PATH. 60s cache (`?refresh=true`) → `200 { accounts, events, errors }`; event `{ event_id, title, start, end, meet_url, live_now, account }` (`live_now` = inside the event window, 5-min early grace); per-account failures go to `errors` without hiding accounts that worked. |
| `POST /meetings/listen` | Passive (system audio + mic; never joins). `{ url?, title?, date?, mic? }`, idempotent per thread → `{ session_id, thread_id, ... }`. |
| `POST /meetings/join` | Attendee. `{ url, title?, date?, display_name?, realtime_profile? }`; id from `GET /media/providers` (the web Join form sends it), default `MEET_BOT_REALTIME_PROFILE`, unknown → `voice_realtime_openai_backend`. `web.whatsapp.com` URL = WhatsApp call. |
| `GET /meetings/whatsapp-ring` | The WhatsApp call already open in everyday Chrome: `{ phase, surface, profile, profiles, url, title, caller, session_id, error, starting, leaving }`. `phase`: `idle`, `incoming`, `ongoing`, `approved`, `listening`, `cancelled`, `failed`, `leaving`; `surface`: `ringing`, `ongoing`, `none`; `caller` nullable; `profiles` from `GET /media/providers` (`{ id, label, available, unavailable_reason? }`); `profile` is one of those ids, stored default `voice_realtime_openai_backend`. |
| `POST /meetings/whatsapp-ring` | `{ decision, url?, title?, caller?, profile? }`; `decision`: `incoming`, `ongoing`, `join` (alias `approved`; ringing only), `listen` (passive Chrome-bundle capture, mic off, from a ring or ongoing call; idle starts nothing), `cancel` (hang up; also stops Magician's session if joined or starting), `ignored` (no phase change), `cleared`, `ended`, `profile`. Unknown `profile` → `voice_realtime_openai_backend`; ignored while starting/live/listening/leaving. Join/Listen do not call `/meetings/join`; the request bearer, when present, is Join's voice-session token. Buttons: [meetings.md](meetings.md#already-open-chrome). |
| `GET /meetings/{id}` | One session, either rail. |
| `POST /meetings/{id}/stop` | Passive `listen-*`: `202` on capture cancel, STT / summary finish after. Attendee leave keeps its teardown contract. |
| `POST /meetings/{id}/pause`, `/resume` | Stop the ears, stay in: passive drops chunks pre-STT; attendee muted (no transcript, no wake replies). |

## Settings

| Route | Description |
| --- | --- |
| `POST /settings/magician-config/reload` | Partial reload of `magician-config.yaml` → `live_reloaded`, `restart_required`, `warnings`. |
| `GET/PUT /settings/trust-policy` | Scoped trust policy. |
| `POST /settings/trust-policy/restore-template` | Shipped template. |
| `GET/PUT /settings/privacy` | `privacy.processing`; PUT writes durably, then runs the reload path. |
| `GET/PUT /settings/critical-delivery` | `hitl.critical_delivery` (channels, order, fan-out, push, quiet hours). GET: enabled channels (masked owner addresses), ownerless channels, secure-link readiness. PUT validates, writes durably, reloads, hands the delivery coordinator its policy; sends nothing. |
| `POST /settings/critical-delivery/test` | Owner's explicit test alert to all enabled destinations (`202`, delivery rows). |
| `GET /hitl/deliveries?correlation_id=` | Value-free rows (masked destinations, states), request→queued / queued→accepted p50/p95, last claim per channel. |
| `POST /hitl/deliveries/{id}/claim`, `…/report` | Bots only (`mag_bot_` bearer; bot name = channel type): claim (owner address + card; binds bot + connection generation), report `provider_accepted` / `confirmed_delivered` / `failed`. See `critical-request-delivery.md`. |
| `GET /hitl/{correlation_id}/retrieval` | Value-free `otp` code retrieval: `waiting` / `code_used` / `ambiguous` / `unavailable` / `stopped` / `none`, source kinds, reason. |
| `PUT /channel-assist/channels/purpose` | `verification_codes` purpose on an Observe account (`{provider, account_alias, granted}`); observation consent grants nothing. |
| `PUT /devices/policy/verification-codes` | Android device as code source (`{device_id, permitted}`); `GET /devices/policy` → `verification_code_devices`. |
| `GET/PUT /settings/local-generation` | Local-generation kitty pin (`runtime.ollama.local_generation.selected`). GET: RAM, tier recommendation, scores, warnings. PUT writes the YAML anchor, reloads, runs `scripts/run-ollama.sh` by default; tier mismatch only warns; the switch still happens. |

Live reload (`live_reloaded`): `operation_llm_router`, `multi_llm_service`, `tool_authorization`, `hitl_critical_delivery` and `hitl_verification_codes` (when their coordinators are running), `api_mining_master_switch` (`api_mining.enabled`), `memory_prompt_budgets`, `coding_budgets`, `execution_budgets`, `llm_content_capture_policy`, `agent_resources_config`, `interactive_process`, `harness_engine`, `chat.harness_engine`, `decision`, `execution.god_mode`, `execution.network_policy`, `planning.analysis_path`. Everything else is `restart_required`: `consumer_mode`, `execution_settings`, `frontend_settings`, `storage_path`, `service_url`, `social`, other `api_mining` settings, `capability_evolution`, `enrollment`, `analytics.llm_trace.payload_records`, `runtime.retrieval.*`, `MAGICIAN_VECTOR_SEARCH`, `runtime.ollama.embedding_query_batch_*`, `llm.dispatch.*` (engine, workers, reserved interactive workers, queue capacities, `max_request_bytes`, queue byte caps), `runtime.scale`, `MAGICIAN_SCALE_PROFILE`.

## Human-in-the-Loop (Canonical)

One respond endpoint; old URLs return `410 Gone` (`error: endpoint_retired`). Events: [v2-websocket-events.md#human-in-the-loop-canonical](v2-websocket-events.md#human-in-the-loop-canonical). Contract: [hitl-attention.md](hitl-attention.md). Frontend: `ui/unified-ui/src/lib/hitl/respondToHitl.ts`.

| Route | Description |
| --- | --- |
| `POST /hitl/{correlation_id}/respond` | `{ "source", "value", "channel"?, "input_type"?, "execution_id"?, "task_id"?, "selected_paths"? }`, bearer scope must match → `{ accepted, source, reason? }` (`already_resolved` `409`, `scope_mismatch` `403`) or the subsystem body. `channel` is attribution except: `verification_code_resolver` is runtime-only (`400 reserved_channel`); `android_notification` needs the paired device's credential (`401 paired_device_required`) and, beyond an ordinary decision (`confirmation`, or `choice` without `other_value`, not typed `otp`/`password`), device policy for verification codes (`403 device_not_permitted_for_verification_codes`). `plan_approval` approved via `ios_notification`, `android_notification`, `ios_push`, `android_push` or `web_push` also starts the run (`reason: run_started` / `run_not_started`). |
| `GET /hitl/deprecation-metrics` | Process-local retired-URL hits: `{ generated_at_ms, metrics: [{ endpoint, hits_total, last_hit_at_ms? }] }`. Handler resolves no scope, but the route is on the v2 wrap, so credentials mode needs a bearer. |

`source` dispatch:

- `agentic` / `primitive` / `inner_loop` / `escalation` — resume the matching runtime pause (`resume_agentic_execution_with_scope`); V3 runs project into task state (`PlanningComplete` → `ready`).
- `user_request` — `UserRequestService::respond_scoped`.
- `approval` — approval-service resolver.
- `clarification` — V3 `submit_task_plan_clarification` first; AskLoop fallback only on `404`. Needs `task_id` (or historical `execution_id`).
- `plan_approval` — approve/reject the plan version (`correlation_id` = `plan_id`).
- `bot_auth` — `correlation_id` `bot_auth:<principal>:<workspace>:<bot>`.
- `mcp_oauth` — retry/dismiss only; no tokens in body.
- `diff_approval` — apply/reject a gated file-edit proposal (`selected_paths` optional).
- `service_health` — dismiss a service-health notice (`choice` `dismiss` only; else `400 service_health_requires_dismiss`; not pending → `409`).

`value` (`AgenticResumeValue`): `text`, `password`, `choice`, `multi_choice`, `confirmation`, `external_action_completed`, `file_path`, `guidance`, `aborted`, `form`.

Retired (still registered, `410`): `POST /executions/{id}/execution/agentic-resume`, `POST /user-requests/{id}/respond`, `POST /approvals/{approval_id}/resolve`, `POST /executions/{execution_id}/clarify/{question_id}/respond`, `POST /api/magician/v3/tasks/{task_id}/plan/clarifications/{question_id}/respond`. Still read: `GET /user-requests` (pending in bearer scope; optional `owner_agent_id`, `include_history`), `GET /approvals`, `GET /approvals/{id}`.

Bodies: `{"source":"agentic","value":{"type":"text","value":"my_username"},"execution_id":"…"}`; choice `{"type":"choice","selected_id":"option_2"}`; `{"source":"user_request","value":{"type":"choice","selected_id":"allow_once"},"channel":"web"}`.

## Browser Web Push

Web Push (RFC 8030/8291/8292) for attention. Scope is the request's own (`X-Principal`/`X-Workspace` fixed by auth middleware); bodies name none. Plain v2 bearer, no cookie. Delivery/pruning as phones: [hitl-attention.md](hitl-attention.md) (Push → Browsers).

| Route | Description |
| --- | --- |
| `GET /web-push/config` | `{ "enabled": bool, "vapid_public_key": string \| null }` (unpadded base64url P-256, `applicationServerKey`); `enabled` needs both VAPID keys. `Cache-Control: no-store`. |
| `PUT /web-push/subscription` | `{ "subscription": { "endpoint", "keys": { "p256dh", "auth" } }, "label"? }` (`PushSubscription.toJSON()`, label ≤120). Idempotent per endpoint → `200 { "id" }`. `400 invalid_web_push_subscription` unless https browser push service (FCM, Mozilla, WNS, Apple) and 65-byte point / 16-byte secret (padded or unpadded, standard or URL-safe base64); `503 web_push_not_configured` without VAPID keys; `429` at capacity. 16 per scope, oldest replaced. |
| `DELETE /web-push/subscription` | `{ "endpoint" }` → `204` whether or not registered. |

Payload `{ kind: "attention_requested", correlation_id, source, expires_at, web_path: "/attention?attention_item=<url-encoded correlation_id>", answer_mode?, option_count? }` (`option_count` numeric) or `{ kind: "attention_resolved" | "attention_expired", correlation_id }`; a withdrawal goes only to a subscription that was sent that ask (no silent pushes). Answers: `/hitl/{correlation_id}/respond`, `channel: "web_push"`.

## Operator feed

`GET /api/magician/v2/updates` — per-workspace `AgentUpdate` JSONL journal, bearer scope (`workspace` ignored). `since` (unix ms), `limit` (200, max 1000), `agent`, `kind`, `thread` → `{ count, events }` newest-first. [v2-websocket-events.md](v2-websocket-events.md#operator-feed-agent-update), [agent-updates.md](agent-updates.md).

## Tasks API (V3)

No `/tasks*` under `/api/magician/v2`; `/tasks/counts`, `/tasks/running`, batch `/tasks/approve`, `/execution-history`, `/defer`, `/pause`, `/stop`, `/execution-results` do not exist. [internal-tasks-api.md](internal-tasks-api.md), [v3-complete-flow.md](v3-complete-flow.md). Prefix `/api/magician/v3`.

| Route | Description |
| --- | --- |
| `POST /tasks` | Non-empty `description`; optional `title`, `depends_on`, `agent_id`, `schedule`, `save_as_task`, `approved` (default `true`: authorizes, never starts). `201` adds `next_step` naming the call that starts it (`/execute`, or `/approve` first); absent when scheduled. |
| `GET /tasks` | `TaskRecord` projections. |
| `GET/PUT/DELETE /tasks/{id}` | Read / metadata / cascade delete; `description` cannot be blanked. |
| `PUT /tasks/{id}/status` | Persists the V3 projection; `pending`/`ready` may reopen a terminal task after settling the root. |
| `POST /tasks/{id}/approve` | One task. |
| `POST/GET/PUT /tasks/{id}/plan` | `PlanGraph` envelope; do-mode `404 task_plan_not_found`. |
| `GET /tasks/{id}/plan/versions`, `/versions/{epoch}` | Versions. |
| `POST /tasks/{id}/plan/versions/{epoch}/restore` | Restore. |
| `POST /tasks/{id}/plan/approve`, `/reject`, `/replan` | Review. |
| `POST /tasks/{id}/analyze` | Standalone analysis. |
| `GET /tasks/{id}/plan/analysis`, `/slots`, `/attempts` | Analysis, slots, attempts. |
| `GET /tasks/{id}/plan/clarifications`, `/clarifications/pending` | History / pending. |
| `POST /tasks/{id}/plan/clarifications/resume` | Manual resume; answers go through HITL (V3 respond URL `410`). |
| `GET /plans/clarifications/pending` | Scope-level pending. |
| `POST /tasks/{id}/execute` | Runtime-context harness; optional `refinement`, `overwrite`, `llm_routing_overrides`, `delegate_to_agent` → `202 { task, execution }`; run key `execution_id`. |
| `GET /tasks/{id}/executions`, `/execution-tree`, `/execution-panel` | History / tree / panel. |
| `GET /tasks/{id}/progress`, `/refs`, `/outputs` | `/outputs` adds `deliverable`, `absolute_path` ([internal tasks API](internal-tasks-api.md)). |
| `GET /tasks/internal` | Internal lifecycle. |

`ready` is the canonical reset/rearm state: a resumed execution settling `PlanningComplete` → `ready`, not `paused`; `WaitingUser` / `Paused` stay `paused` until real continuation; `WaitingChildren` stays `running`. HITL `agentic` resume, `agentic-continue` and `PUT /tasks/{id}/status` persist this immediately.

## Bots

UI `/presto/bots`; auth: [bot-auth-contract.md](bot-auth-contract.md).

| Route | Description |
| --- | --- |
| `GET /bots` | Configured processes: runtime/desired state, restarts, QR flag. |
| `POST /bots/{name}/start`, `/stop`, `/restart` | Control. |
| `GET /bots/{name}/logs` | Recent stdout/stderr; `limit` (default 100). |
| `GET /bots/{name}/qr` | QR image, when the bot exposes one. |
| `GET/PUT/DELETE /bots/{name}/config` | `bots:` entry; PUT syncs the manager. |
| `GET /bots/auth` | Bulk `BotAuthSnapshot` (AttentionBar `NeedsAuth`; records HITL snapshots). |
| `GET /bots/auth/state` | Scope-wide. |
| `GET /bots/{name}/auth` | One bot. |
| `POST /bots/{name}/auth/start` | Adapter login (`202`). |

## Agents and programs

| Route | Description |
| --- | --- |
| `GET /programs` | `{ programs: [{name, title}] }` from `programs/*.md`. |
| `GET /programs/{name}` | Document + managed `## Missions (CEO)` + `.history/` + content `sha256`. `404`/`400` (bare `*.md` only). |
| `PUT /programs/{name}` | Owner session or the owner's paired iOS/Android app; agent grants refused. `{ content, base_sha256? }`: base is the GET `sha256`, required for an existing program, omitted to create. Replaced content snapshotted to `.history/` first; unchanged saves write nothing. `409 program_changed` + `current_sha256` when the document changed, appeared or vanished since it was read. |
| `POST /programs/{name}/revert` | `{ snapshot? }` (newest); current content snapshotted first. |
| `POST/GET /agents` | Create (`201` + `Location`) / list ambient. Surface-only omitted; disabled stay listed for re-enabling. |
| `GET /agents/health` | Crew health. |
| `POST /agents/refresh-definitions` | Reload. |
| `GET /agents/{id}` | With `ETag`. Self allowed; others need the reachable-target set. |
| `GET /agents/{id}/effective-tools` | Tool-policy snapshot, `?surface=chat\|realtime_voice\|tutor\|app_copilot\|thinking_map`; no schemas or parameter-deny values. |
| `PUT/PATCH/DELETE /agents/{id}` | Replace / RFC 7386 merge / `204`; PUT/PATCH need `If-Match` (e.g. `If-Match: "v1"`). |
| `POST /agents/{id}/set-primary` | Primary personal agent. |
| `POST /agents/{id}/trigger` | Manual goal cycle; disabled/surface-only rejected before persist. |
| `POST /agents/{id}/pause`, `/resume` | Pause / resume. |
| `GET/PUT /harness/runtime` | Effective global harness state. PUT `{ "enabled": true\|false }` persists `harness.paused`; disabling clears queued `harness:`-goal starts, cancels only harness cycles ([harness-sre-agent.md](harness-sre-agent.md); `active_cycles_targeted` counts them), drops `MAGICIAN_HARNESS_PAUSED` from live `.env` / `.env.development`. |
| `GET /harness/cycles` | Recent `harness_cycle_dispatch_outcome` events + tallies. |
| `GET /harness/anomalies` | Persisted anomalies; optional `status=open\|fix_dispatched\|resolved\|dismissed`. |

`GET /agents` is a policy projection, not an execution grant or model/composer authority; product features use typed routes (Thinking Map: `surface=thinking_map`, `feature_mode=brainstorm`).

## Skills

`skills_api::configure_skills_routes`; no promote/demote. [skills-quickstart.md](skills-quickstart.md).

| Route | Description |
| --- | --- |
| `GET /skills` | Installed procedure + personality-mode skills, embedded compiled packs (`kind: compiled`, `layer: built-in`), workspace-layer entries from the bearer scope. |
| `POST /skills/install` | Operator-privileged, `X-Magician-Setup-Token`. `{ "source": "path:<abs>"\|"skillshub:<name>", "target": { "workspaces": ["principal/workspace"] } }`; no system-shared tier. |
| `POST /skills/{name}/allow-for-agent` | Agent allow-list; same gate. |
| `GET /skills/{name}/schema` | `tool_schema.yaml` (workspace wins). |
| `POST /skills/{name}/run` | CLI dispatch, no LLM. `{ action?, args?, session_params?, timeout_secs? }`; `implementation.command` (`browser` → `agent-browser`) + action `argv`, `MAGICIAN_SKILL_DIR`, `<skill_dir>/bin` on `PATH` → `{ prelude, argv, exit_code, success, stdout, stderr, duration_ms, parsed_json }`. `timeout_secs` default 60; the child is killed on timeout or drop. |

## API mining

`ApiMiningApi` over `CapabilityRegistry`, the captured-auth `SecretStore` partition and the persisted origin-policy store. [api-mining-pipeline.md](api-mining-pipeline.md), [api-explorer.md](api-explorer.md).

| Route | Description |
| --- | --- |
| `GET /api-mining/registry` | `RegistryIndex` (`version`, `origins`, `last_rebuilt`); `404` if unconfigured. |
| `GET /api-mining/noisy-origins` | ≥25 stored trace events (not only promoted capabilities), with operator decision: `{ threshold, origins: [{ origin_key, origin_url, trace_count, capability_count, decision }] }`. |
| `GET /api-mining/capabilities/{origin_key}/{capability_id}` | `ApiCapability`; `origin_key` filesystem-safe (`https___api_github_com`). |
| `GET /api-mining/auth-status/{origin_key}` | Non-secret metadata `{ has_auth, is_stale, has_cookies, has_headers, has_storage }`. |
| `POST /api-mining/origins/{origin_key}/refresh-auth` | Scoped Magicutor CDP capture, no LLM. `202` new / `200` reuse; `400` unless `http`/`https`. |
| `GET /api-mining/origins/{origin_key}/refresh-auth/{refresh_id}` | Non-secret refresh phase. |
| `POST /api-mining/replay/{origin_key}/{capability_id}` | Replay, optional overrides. |
| `GET /api-mining/openapi/{origin_key}` | OpenAPI 3.0. |
| `POST /api-mining/origins/{origin_key}/allow`, `/block`, `/purge`, `/block-and-purge` | Policy; trace-only origins send `{ "origin_url": "https://…" }`. |

## Unified Event Stream — `GET /api/magician/v3/events`

`GET /api/magician/v3/events?<filters>`: live `RuntimeTransportEvent` tail from the runtime broadcaster plus optional disk backfill, NDJSON (`application/x-ndjson`); same events as `/realtime/ws`. Persistence: [v2-websocket-events.md#event-persistence--retention](v2-websocket-events.md#event-persistence--retention). Filters (AND):

| Param | Description |
| --- | --- |
| `execution_id`, `task_id` | Both: per-execution `events.jsonl` backfill; neither or `task_id` alone: cross-scope (per-scope log + recent per-execution files). |
| `since` | Earliest `timestamp_ms`; none per-execution, cross-scope defaults to now−24h (`since=0` widens). |
| `before` | Backfill `timestamp_ms < before`; tail unaffected. |
| `limit` | Backfill cap; default `EVENTS_RETENTION_MIN_COUNT` (2000). |
| `category` | Comma-separated `EventCategory` tokens (`pipeline,plan,tool,slot,clarification,execution,llm,agentic,hitl,agent,task,feed,observability,media,activity`); unknown dropped. |
| `severity` | `info,warn,error,decision,attention`. |
| `user_relevant` | `true` / `false`; omitted = all. |
| `event_type` | Substring. |
| `agent_id` | Equality. |
| `search` | Substring of serialized JSON. |
| `backfill_only` | `true` closes after backfill (history snapshots); default keeps the live tail open. |

Synthetic rows:

- `{"event_type":"__events_partial__","scanned":N,"scan_cap":M,"message":"…"}` — cross-scope backfill hit the 50k scan cap.
- `{"event_type":"__events_lagged__","skipped":N,"timestamp_ms":…,"message":"…"}` — live-tail broadcaster dropped events for this request (in-flight only).
- `{"event_type":"__transport_log_lagged__","skipped":N,"timestamp_ms":…,"message":"…"}` — on-disk writer lagged; the gap is permanent for that window.

Backfill: per-execution reads `…/scopes/{principal}/{workspace}/tasks/{task_id}/executions/{exec_id}/events.jsonl`; cross-scope reads `…/scopes/{principal}/{workspace}/events.jsonl` (to its length at open) plus per-execution files newer than `since`, `timestamp_ms` descending in a bounded top-`limit` accumulator (default 2000, not the 50k scan cap); `?task_id=` resolves that task's executions. Both legs check client disconnect every 256 lines. Workspace filter fails closed (`event_visible_to_scope`); the tail races `recv()` against the response `closed()` future, so disconnects release immediately.

## Common workflows

### Create → message → inspect

```bash
H=(-H 'Content-Type: application/json' -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN")
B=http://127.0.0.1:3002/api/magician/v2
EXECUTION_ID=$(curl -s -X POST $B/executions "${H[@]}" -d '{"title":"Diagnostics"}' | jq -r '.execution_id')
curl -s -X POST $B/executions/$EXECUTION_ID/message "${H[@]}" -d '{"text":"Check if google.com is reachable","skip_planning":true}'
curl -s "$B/executions/$EXECUTION_ID/turns?direction=Inbound" "${H[@]}"
```

### Clarifications and pauses

`GET /api/magician/v3/tasks/$TASK_ID/plan/clarifications/pending`, answer via `POST /api/magician/v2/hitl/{correlation_id}/respond`, resume via `POST /api/magician/v3/tasks/$TASK_ID/plan/clarifications/resume`. Tree pause is `/executions/{id}/pause` / `/resume`, not HITL.

## Errors

`ApiErrorEnvelope` (`magician-api/src/web_api.rs`): `{ "code": "invalid_request", "error": "Human readable message", "details"? }`.

| Status | Meaning |
| --- | --- |
| `400` | Validation (`invalid_request`). |
| `401` | Missing/invalid bearer (`WWW-Authenticate: Bearer`). |
| `403` | HITL/control scope mismatch. |
| `404` | `resource_not_found`; cross-scope looks identical. |
| `409` | Conflict (steer queue full, `already_resolved`). |
| `410` | Retired HITL URL. |
| `500` | `internal_error`. |

- **Pagination**: `limit` 50 (max 200 on execution/turn lists); follow `pagination.has_more`.
- **Idempotency**: writes are not idempotent unless the route says so (e.g. `PUT /web-push/subscription`, `POST /meetings/listen`); dedupe retries.
- **Concurrency**: the file-backed store uses per-execution locks; pause/resume/cancel share the root-id lock.
- **Scope**: never send `principal`/`workspace` as authority; `anonymous`/`default` exist only for local open-mode bootstrap.
- **Streaming**: `/realtime/ws` or `GET /api/magician/v3/events`; analysis polling (`/analyze`, `/executions/{id}/analysis`) is removed.

## Related references

- `magician-bin/src/main.rs` — Actix scopes (`/api/magician/v2`, `/api/magician/v3`).
- `magician-api/src/` — `web_api.rs`, `task_api_v3.rs`, `events_api.rs`, `chat_api.rs`, `meetings_api.rs`, ….
- `magician/src/magician_v2/storage/models.rs` — `ExecutionRun`, `V2Turn`, `WaitingState`.
- `magician/tests/magician_v2_api_integration_test.rs` — HTTP integration coverage.
- [Auth](auth.md), [HITL](hitl-attention.md), [V3 flow](v3-complete-flow.md), [WebSocket events](v2-websocket-events.md), [ARCHITECTURE_V2.md](../../ARCHITECTURE_V2.md).
