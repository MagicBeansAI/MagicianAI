# Magician V2 API Guide

The Magician HTTP service is the `magician` binary (`magician-bin`). Routes are registered in `magician-bin/src/main.rs`; handlers live in `magician-api`. By default the process binds **http://127.0.0.1:3002** (`--host` / `--port` override). It is not bound to all interfaces unless you pass `--host 0.0.0.0`.

Two authenticated scopes share the same bearer middleware:

- **V2** `http://127.0.0.1:3002/api/magician/v2` — executions, chat, meetings, agents, bots, skills, HITL, settings, API mining.
- **V3** `http://127.0.0.1:3002/api/magician/v3` — tasks, plans, monitors, and the NDJSON event tail.

This page is the orchestrator REST contract. Surfaces with their own docs (apps, feed, learning, media, plane, devices, auth internals) are linked, not duplicated. Unused JSON fields may appear on the wire; do not treat extra keys as a contract.

- **Protocol**: JSON/REST. Realtime: `GET /api/magician/v2/realtime/ws` (see [V2 WebSocket Events](v2-websocket-events.md)).
- **Authentication**: workspace-bound bearer. Details: [auth.md](auth.md).

---

## Auth

Except for documented public doors (`POST /auth/login`, social start/callback, `POST /chat/enroll/approve`, one-time device enrollment exchange, ESP `POST /devices/pair`, `/plane/mcp`), clients send `Authorization: Bearer <token>`. Every token is minted for one principal/workspace. `X-Principal`, `X-Workspace`, and `principal`/`workspace` query or body selectors are not client authority and must not be sent as such. Browser WebSockets offer a stable application protocol (`magician-events-v2` for realtime or `magician-voice-control-v1` for voice control) followed by the same opaque bearer as `magician-bearer.<token>`. Middleware consumes the bearer; the server selects only the application protocol and never echoes the credential.

In `auth.mode: open` with an empty identity store, a missing bearer is fixed to `anonymous`/`default`. After the first identity exists, bearerless requests are refused.

---

## Data model

Wire types: `magician/src/magician_v2/storage/models.rs`.

### ExecutionRun

`GET /executions/{id}` returns this object (not a full document with turns/slots). Create returns `{ execution_id, execution, initial_message_enqueued, skip_planning }`.

```json
{
  "id": "execution-uuid",
  "principal": "user-123",
  "workspace": "workspace-a",
  "task_id": "task-uuid",
  "title": "Network Diagnostics",
  "waiting_state": "Planning",
  "created_at": 1719860000000,
  "updated_at": 1719860055000,
  "processing_correlation_id": null,
  "current_stage": "planning_bootstrap",
  "current_provider": "openai",
  "active_owner_agent_id": "personal-assistant",
  "owner_stack": [],
  "entry_mode": "planning_backed"
}
```

| Field | Notes |
| --- | --- |
| `id`, `principal`, `workspace` | Scope is bearer-derived; `principal`/`workspace` on the record are storage, not request authority. |
| `task_id`, `root_execution_id`, `parent_execution_id`, `child_execution_ids` | Task-backed tree links. |
| `title` | Optional. |
| `waiting_state` | `Planning`, `PlanningComplete`, `Runnable`, `Executing`, `Sleeping`, `WaitingChildren`, `WaitingUser`, `Completed`, `Failed`, `Cancelled`, `Paused`. `Sleeping` is a durable timer, not a user pause. |
| `created_at`, `updated_at` | Unix ms. |
| `processing_correlation_id` | In-flight orchestration, if any. |
| `current_stage`, `current_provider` | Optional telemetry. |
| `escalation_trigger` | Set when paused by `CannotProceed` / `LoopDetected`. |
| `active_owner_agent_id`, `owner_stack`, `active_delegation_group`, `delegation_chain` | Ownership / handover. |
| `work_authority` | Inherited work context; absent means unbound, not unrestricted. |
| `paused_from_state` | Exact pre-pause `WaitingState`. |
| `entry_mode` | `planning_backed` or `direct`. |

Clarification session/history live on `ExecutionRunDocument`, not on `ExecutionRun`. There is no registered `/slots` REST surface; slots remain a storage field on the document (`pending_slots_count` still appears on list summaries).

### V2Turn

```json
{
  "id": "turn-uuid",
  "execution_id": "execution-uuid",
  "direction": "Inbound",
  "text": "Can you check if google.com is reachable?",
  "in_reply_to_slot_id": null,
  "created_at": 1719860000123,
  "query_analysis": {},
  "recommended_questions": [],
  "enriched_query": "Check reachability of google.com over ICMP"
}
```

| Field | Notes |
| --- | --- |
| `id`, `execution_id`, `direction`, `text`, `created_at` | `direction` is `Inbound` or `Outbound`. |
| `in_reply_to_slot_id` | Optional. |
| `query_analysis`, `analysis_metadata`, `strategy_attempts`, `processing_metadata` | Planner/pipeline metadata. |
| `recommended_questions` | Optional elicitation questions. |
| `enriched_query` | Optional **string** (rewritten query), not an intent/entity object. |

### Execution list row

`GET /executions` returns `PaginatedResult<ExecutionSummary>`: `items` plus `pagination.{total,limit,offset,has_more}`. Summaries include `turn_count`, `pending_slots_count`, owner/delegation fields, and optional `escalation_trigger`. Default `limit` 50, max 200, sorted by `updated_at` descending.

---

## Health and mode

These three are **outside** the `/api/magician/v2` auth wrap:

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/health` | Process liveness plus best-effort Magicutor / desktop probes. |
| `GET` | `/health/storage` | Storage profile, adapter health, scope-lease generations. `503` when a lease is lost. |
| `GET` | `/health/execution-driver` | Resolved `MAGICIAN_EXECUTION_DRIVER`, unrecognised-value flag, run counts since start. |

`GET /api/magician/mode` (also outside v2) returns `{ "mode": "standard", "timestamp": "<rfc3339>" }`.

---

## Executions

Prefix `/api/magician/v2`. Control routes (`cancel`, `abort`, `pause`, `resume`, `steer`, and the active-execution delete alias) require a workspace-bound bearer; the execution must belong to that scope or the lookup is not found.

| Method | Path | Description |
| --- | --- | --- |
| `POST` | `/executions` | Create a task-backed execution shell. Optional body: `title`, `initial_message`, `ui_thread_id`, `skip_planning`, `max_iterations`, `llm_routing_overrides`, `env_mode`, `debug`, `internal`, `engagement_id`. Scope comes from the bearer. `internal: true` routes the V3 task to Internal without debug/`__system__` provenance; `debug: true` also applies debug provenance. With `initial_message`, the launch is accepted durably and the response is `202`; otherwise `200`. Envelope: `{ execution_id, execution, initial_message_enqueued, skip_planning }`. |
| `GET` | `/executions` | Paginated list. Query: `limit`, `offset`. `workspace` is ignored. |
| `GET` | `/executions/{id}` | One `ExecutionRun`. |
| `DELETE` | `/executions/{id}` | Cancel the tree, then delete. `204`. |
| `GET` | `/executions/{id}/status` | `{ execution_id, waiting_state }`. |
| `PUT` | `/executions/{id}/status` | `{ "status": "Paused" }` (and other `WaitingState` values). |
| `GET` | `/executions/{id}/control-state` | Server-authoritative `can_pause`, `can_resume`, `can_steer`, `can_cancel` for the active tree. Fails closed while another tree mutation is settling. |
| `POST` | `/executions/{id}/start` | Direct agentic start on an existing run. Body: `{ "goal": "...", "max_iterations"?, "llm_routing_overrides"? }`. |
| `POST` | `/executions/{id}/cancel` | Cancel in-flight orchestration. |
| `POST` | `/executions/{id}/abort` | Alias for `/cancel`. |
| `DELETE` | `/executions/{id}/execution` | Same cancel handler (stop a live agent without deleting the execution record). |
| `POST` | `/executions/{id}/pause` | Pause the whole active tree (`pause_execution_tree`). Success only after every running node has a durable exact-resume checkpoint and `Paused`. `{ paused: true }` / invalid when not live. |
| `POST` | `/executions/{id}/resume` | Resume a paused tree (`resume_execution_tree`). Validates the tree and every required checkpoint before consuming one. `{ resumed: true }` / invalid when not resumable. |
| `POST` | `/executions/{id}/steer` | Operator redirect. Body `{ "message": "..." }` is drained FIFO into the next decision turn as additive `[OPERATOR STEER]` guidance. Cap 4 KiB and 16 pending; a full queue is `409`. `{ steered: true }`. |
| `POST` | `/executions/{id}/execution/agentic-continue` | Continue after a max-iterations (or similar) pause. Optional `pause_state_id` / plan/step / agent/goal/cycle keys, plus optional `guidance`. Not a HITL answer. |
| `POST` | `/executions/{id}/execution/agentic-cancel` | Cancel a paused agentic run. |
| `GET` | `/executions/{id}/execution-summary` | Latest persisted agentic summary (`{ execution_id, summary }`). |
| `GET` | `/executions/{id}/responsibility` | Owner chain, handover flag, blocking children, `responsibility_summary`. |
| `GET` | `/executions/{id}/execution-panel` | Execution-panel projection (also registered under V3). |
| `GET` | `/executions/{id}/pause-state` | Debug pause-store snapshot. |
| `GET` | `/executions/{id}/observations` | Observation list. |
| `GET` | `/executions/{execution_id}/observations/{observation_id}/screenshot` | Stored screenshot. |
| `GET` | `/executions/{execution_id}/observations/{observation_id}/json` | Raw observation JSON. |
| `GET` | `/executions/{execution_id}/storage-stats` | Storage stats. |

Pause, resume, and cancel are serialized by the resolved root execution id. Runtime status transitions share per-execution locks with control mutations. Manual-pause checkpoints must be durable before `Paused`; persistence failure fails closed.

`POST /api/magician/v2/admin/tasks/reconcile-orphaned-ready` is the operator repair for agent-created tasks left orphaned in `ready`. Defaults `dry_run: true`; `batch_cap` 1–100 (default 25); scope comes from the request headers (optional body `workspace`); applying answers 409 `auto_dispatch_disabled` while agent-created task auto-dispatch is off. Matching CLI: `magician reconcile-orphaned-tasks [--api-base <url>] [--batch-cap <n>] [--apply]` (bearer from `MAGICIAN_BEARER_TOKEN`).

There is one runtime harness: with or without a stored `PlanGraph`, execution is the direct agentic loop (the graph, if present, is advisory context). The API does not accept a strategy selector.

### Turns

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/executions/{id}/turns` | Paginated history. Query: `direction=Inbound\|Outbound`, `limit`, `offset`. |
| `POST` | `/executions/{id}/message` | Append an inbound turn. Body: `{ "text": "...", "in_reply_to_slot_id"?, "skip_planning"?, "max_iterations"?, "llm_routing_overrides"? }`. |

There is no `/executions/{id}/analysis` or `/analysis/{turn_id}` route. Query analysis is task-scoped on V3 (`GET /api/magician/v3/tasks/{task_id}/plan/analysis`). Standalone `/analyze` is gone (`POST /api/magician/v3/tasks/{task_id}/analyze`).

---

## Chat and consumer channels

Shared backend for the web client and channel bots. See [Consumer Channels](../../consumer_channels.md) and [chat-mode.md](chat-mode.md).

| Method | Path | Description |
| --- | --- | --- |
| `POST` | `/chat/enroll` | Enroll or re-enroll a channel identity in the bearer workspace. Body: `{ "channel_type", "channel_address", "display_name"? }`. Auto-approved `{ enrolled: true, principal }` or `{ enrolled: false, code }` when approval is required. |
| `POST` | `/chat/enroll/approve` | Public admin door: `Authorization: Bearer` is `MAGICIAN_ADMIN_SECRET`, not a `mag_` token. Body: `{ "code", "principal", "workspace"? }` (`workspace` defaults to `default`). |
| `POST` | `/chat/enroll/revoke` | Un-enroll. Holder may revoke their own channel; owner may revoke anyone's. Body: `{ "channel_type", "channel_address" }`. |
| `POST` | `/chat/enroll/cancel` | Cancel a pending enrollment via the one-time `code`, or as owner via channel identity. |
| `GET` | `/chat/enroll/status` | `channel_type` + `channel_address` in the bearer workspace. |
| `GET` | `/chat/active` | Get or create the active session. Optional `channel` + `channel_address`. |
| `POST` | `/chat/new` | Archive the current active session and create a fresh one. |
| `GET` | `/chat/sessions` | List sessions. |
| `GET` | `/chat/sessions/{id}` | Session plus history. |
| `PATCH` | `/chat/sessions/{id}` | Metadata (title, …). |
| `GET`/`POST` | `/chat/sessions/{id}/messages` | List / send. Send returns the assistant `ChatResponse`. |
| `POST` | `/chat/sessions/{id}/transcript` | Display-only live transcript line (`system`). Never dispatches the agent. Archived sessions `400`. |
| `DELETE` | `/chat/sessions/{id}/run` | Cancel an in-flight chat turn. |

---

## Meetings

Two rails, one thread identity: a passive listen and an agent join of the same meeting converge on the same dated `meeting-*` chat thread. Scope from the bearer.

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/meetings` | Active capture sessions (both rails) plus recent `meeting-*` threads (index-backed `ui_thread_id` prefix). |
| `GET` | `/meetings/active` | Live sessions only. Ended sessions remain queryable via `/meetings/{id}` for a short window. |
| `GET` | `/meetings/upcoming` | Next ~12h of calendar meetings across configured GWS accounts, queried concurrently, merged (deduped by meet link / title+start), sorted by start. Account order: `MEET_BOT_CALENDAR_ACCOUNTS` → legacy `MEET_BOT_CALENDAR_ACCOUNT` → `gws_accounts` in the runtime operator-config (resolved via `runtime_config_path("operator-config.yaml", "skillshub/operator-config.yaml")`, filtered to profiles that exist under the scope capability auth root `<scope>/auth/gws-<name>` — the same store the `calendar` skill and GWS bots use) → `work`. Binary: `GWS_BINARY` → skillshub-local → PATH. 60s cache; `?refresh=true` busts it. `200 { accounts, events, errors }`; each event `{ event_id, title, start, end, meet_url, live_now, account }` (`live_now` = inside the event window, 5-min early-join grace). Per-account failures land in `errors` without hiding accounts that worked. |
| `POST` | `/meetings/listen` | Passive listener (local system-audio + mic; never joins). Body `{ url?, title?, date?, mic? }`. Idempotent per resolved thread: a live listener is returned instead of duplicated. `{ session_id, thread_id, ... }`. |
| `POST` | `/meetings/join` | Agent attendee. Body `{ url, title?, date?, display_name? }`. |
| `GET` | `/meetings/{id}` | One session, either rail. |
| `POST` | `/meetings/{id}/stop` | Passive `listen-*` returns `202` as soon as capture is cancelled; tail STT / summary continue. Attendee leave keeps its post-teardown contract. |
| `POST` | `/meetings/{id}/pause`, `/resume` | Pause is “stop the ears, stay in the meeting”: passive drops chunks before STT; attendee is mute (no transcript, no wake replies). |

---

## Settings

| Method | Path | Description |
| --- | --- | --- |
| `POST` | `/settings/magician-config/reload` | Partial live reload of `magician-config.yaml`. Response: `live_reloaded`, `restart_required`, `warnings`. |
| `GET`/`PUT` | `/settings/trust-policy` | Scoped trust-policy document. |
| `POST` | `/settings/trust-policy/restore-template` | Replace with the shipped template. |
| `GET`/`PUT` | `/settings/privacy` | `privacy.processing` switch; PUT writes durably then runs the same reload path. |
| `GET`/`PUT` | `/settings/critical-delivery` | `hitl.critical_delivery`: which verified channels get a critical-request alert, in what order, fan-out policy, push, quiet hours. GET reports enabled channels with masked owner addresses, channels without an owner, whether the secure link can be built. PUT validates, writes durably, reloads, and hands the delivery coordinator its policy. Sends nothing. |
| `POST` | `/settings/critical-delivery/test` | The owner's explicit test alert through every enabled destination (`202`, the delivery rows). |
| `GET` | `/hitl/deliveries?correlation_id=` | Value-free delivery status: rows with masked destinations and states, request→queued and queued→accepted p50/p95, last claim per channel. |
| `POST` | `/hitl/deliveries/{id}/claim`, `…/report` | Channel bots only (`mag_bot_` bearer; bot name = channel type): claim one queued delivery (returns the owner address + card, binds the record to the bot and its connection generation), then report `provider_accepted` / `confirmed_delivered` / `failed`. See `critical-request-delivery.md`. |
| `GET` | `/hitl/{correlation_id}/retrieval` | Value-free status of automatic verification-code retrieval for one `otp` ask: `waiting` / `code_used` / `ambiguous` / `unavailable` / `stopped` / `none`, the source kinds watched, a reason. |
| `PUT` | `/channel-assist/channels/purpose` | Grant or withdraw the `verification_codes` purpose on one configured Observe account (`{provider, account_alias, granted}`); observation consent alone grants nothing. |
| `PUT` | `/devices/policy/verification-codes` | Permit or withdraw a paired Android device's notifications as a verification-code source (`{device_id, permitted}`); `GET /devices/policy` reports `verification_code_devices`. |
| `GET`/`PUT` | `/settings/local-generation` | Local-generation kitty pin (`runtime.ollama.local_generation.selected`). GET reports host RAM, the RAM-tier recommendation, catalog scores, and per-model warnings. PUT writes the YAML anchor, reloads magician-config, and by default runs `scripts/run-ollama.sh`. RAM-tier mismatches are warnings; the switch still happens. |

Live reload refreshes `operation_llm_router`, `multi_llm_service`, `tool_authorization`, `memory_prompt_budgets`, `coding_budgets`, `llm_content_capture_policy`, `agent_resources_config`, `interactive_process`, `harness_engine`, and `chat.harness_engine`. `restart_required` is the contract for the rest, including `consumer_mode`, `execution_settings`, `frontend_settings`, `storage_path`, `service_url`, `social`, `api_mining`, `capability_evolution`, `enrollment`, `analytics.llm_trace.payload_records`, `runtime.retrieval.*`, `MAGICIAN_VECTOR_SEARCH`, `runtime.ollama.embedding_query_batch_*`, `llm.dispatch.*` (engine/workers/reservation/lane caps/`max_request_bytes`/queue byte caps), `runtime.scale`, and `MAGICIAN_SCALE_PROFILE`.

---

## Human-in-the-Loop (Canonical)

The historically separate respond URLs are retired (`410 Gone` + `error: endpoint_retired`). Submit through one endpoint. Event envelope: [v2-websocket-events.md#human-in-the-loop-canonical](v2-websocket-events.md#human-in-the-loop-canonical). Operator contract: [hitl-attention.md](hitl-attention.md). Frontend wrapper: `ui/unified-ui/src/lib/hitl/respondToHitl.ts`.

| Method | Path | Description |
| --- | --- | --- |
| `POST` | `/hitl/{correlation_id}/respond` | Canonical respond. Body `{ "source", "value", "channel"?, "input_type"?, "execution_id"?, "task_id"?, "selected_paths"? }`. Bearer scope must match. Returns `{ accepted, source, reason? }` (`already_resolved` → `409`, `scope_mismatch` → `403`) or the dispatched subsystem body. `channel` is attribution, except two reserved names: `verification_code_resolver` is the runtime's own and is refused from HTTP (`400 reserved_channel`); `android_notification` is accepted only over the paired device's own credential (`401 paired_device_required`) and only while the owner's device policy lists that device for verification codes (`403 device_not_permitted_for_verification_codes`). |
| `GET` | `/hitl/deprecation-metrics` | Process-local hit counts for retired URLs: `{ generated_at_ms, metrics: [{ endpoint, hits_total, last_hit_at_ms? }] }`. The handler does not resolve scope; the route still sits on the v2 wrap, so credentials-mode needs a bearer. |

`source` dispatch (handler match):

- `agentic` / `primitive` / `inner_loop` / `escalation` — resume the matching runtime pause (`resume_agentic_execution_with_scope`). For V3 task-backed runs the outcome is projected into canonical task/execution state (`PlanningComplete` → task `ready`).
- `user_request` — `UserRequestService::respond_scoped`.
- `approval` — approval-service resolver.
- `clarification` — V3 `submit_task_plan_clarification` first; AskLoop fallback only on `404`. Needs `task_id` (or historical `execution_id`).
- `plan_approval` — approve/reject the plan version (`correlation_id` is `plan_id`).
- `bot_auth` — `correlation_id` `bot_auth:<principal>:<workspace>:<bot>`.
- `mcp_oauth` — retry/dismiss only; no tokens in the body.
- `diff_approval` — apply/reject a gated file-edit proposal (`selected_paths` optional).

`value` is `AgenticResumeValue`: `text`, `password`, `choice`, `multi_choice`, `confirmation`, `external_action_completed`, `file_path`, `guidance`, `aborted`, `form`.

Retired (still registered; `410`):

- `POST /executions/{id}/execution/agentic-resume`
- `POST /user-requests/{id}/respond`
- `POST /approvals/{approval_id}/resolve`
- `POST /executions/{execution_id}/clarify/{question_id}/respond`
- `POST /api/magician/v3/tasks/{task_id}/plan/clarifications/{question_id}/respond`

`GET /user-requests` still lists pending requests in the bearer scope (optional `owner_agent_id`, `include_history`). `GET /approvals` / `GET /approvals/{id}` still read.

```bash
# Agentic typed input
curl -s -X POST http://127.0.0.1:3002/api/magician/v2/hitl/$CORRELATION_ID/respond \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -d '{"source":"agentic","value":{"type":"text","value":"my_username"},"execution_id":"'"$EXECUTION_ID"'"}'

# Choice
curl -s -X POST http://127.0.0.1:3002/api/magician/v2/hitl/$CORRELATION_ID/respond \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -d '{"source":"agentic","value":{"type":"choice","selected_id":"option_2"},"execution_id":"'"$EXECUTION_ID"'"}'

# Tool/sandbox user-request
curl -s -X POST http://127.0.0.1:3002/api/magician/v2/hitl/$REQUEST_ID/respond \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -d '{"source":"user_request","value":{"type":"choice","selected_id":"allow_once"},"channel":"web"}'
```

---

## Operator feed

`GET /api/magician/v2/updates` — time-ordered slice of the per-workspace `AgentUpdate` JSONL journal. Scope is the verified bearer (`workspace` query is ignored). Query: `since` (unix ms), `limit` (default 200, max 1000), `agent`, `kind`, `thread`. Returns `{ count, events }` newest-first. See [v2-websocket-events.md](v2-websocket-events.md#operator-feed-agent-update) and [agent-updates.md](agent-updates.md).

---

## Tasks API (V3)

There are **no** `/tasks*` routes under `/api/magician/v2`. Task CRUD, planning, and execute live under `/api/magician/v3` (prefix below). Paths such as `/tasks/counts`, `/tasks/running`, batch `/tasks/approve`, `/execution-history`, `/defer`, `/pause`, `/stop`, and `/execution-results` are not registered. Approve is per-task. Internal listing is documented in [internal-tasks-api.md](internal-tasks-api.md); flow in [v3-complete-flow.md](v3-complete-flow.md).

Prefix `/api/magician/v3`.

| Method | Path | Description |
| --- | --- | --- |
| `POST` | `/tasks` | Create. Requires non-empty `description`; `title` is optional. Optional `depends_on`, `agent_id`, `schedule`, `save_as_task`. |
| `GET` | `/tasks` | List `TaskRecord` projections. |
| `GET`/`PUT`/`DELETE` | `/tasks/{id}` | Get / update metadata / delete (cascade). `description` cannot be cleared to blank. |
| `PUT` | `/tasks/{id}/status` | Status update; persists the canonical V3 projection. Explicit `pending`/`ready` may reopen a terminal task after settling the active root. |
| `POST` | `/tasks/{id}/approve` | Approve one task. |
| `POST`/`GET`/`PUT` | `/tasks/{id}/plan` | Start / read / edit the `PlanGraph` envelope. Do-mode tasks `404 task_plan_not_found`. |
| `GET` | `/tasks/{id}/plan/versions`, `/versions/{epoch}` | Version history. |
| `POST` | `/tasks/{id}/plan/versions/{epoch}/restore` | Restore a version. |
| `POST` | `/tasks/{id}/plan/approve`, `/reject`, `/replan` | Plan review. |
| `POST` | `/tasks/{id}/analyze` | Standalone analysis. |
| `GET` | `/tasks/{id}/plan/analysis`, `/slots`, `/attempts` | Analysis, slot graph, strategy attempts. |
| `GET` | `/tasks/{id}/plan/clarifications`, `/clarifications/pending` | History / pending questions. |
| `POST` | `/tasks/{id}/plan/clarifications/resume` | Manual resume. **Respond** is HITL only (the V3 respond URL is `410`). |
| `GET` | `/plans/clarifications/pending` | Scope-level pending plan questions. |
| `POST` | `/tasks/{id}/execute` | Start the runtime-context harness. Optional `refinement`, `overwrite`, `llm_routing_overrides`, `delegate_to_agent`. `202` `{ task, execution }`. Durable run key is `execution_id`. |
| `GET` | `/tasks/{id}/executions`, `/execution-tree`, `/execution-panel` | History / tree / panel. |
| `GET` | `/tasks/{id}/progress`, `/refs`, `/outputs` | Progress, refs, outputs. |
| `GET` | `/tasks/internal` | Internal-lifecycle list. |

`ready` is the canonical reset/rearm state for V3 task-backed runs. Resumed executions that settle `PlanningComplete` project to `ready`, not `paused`. `WaitingUser` / `Paused` remain `paused` until a real continuation advances the run; `WaitingChildren` stays `running`.

HITL `agentic` resume, execution-owned `agentic-continue`, and `PUT /tasks/{id}/status` all persist that projection immediately.

---

## Bots

Control plane for managed channel bots. UI: `/presto/bots`. Auth sidecar: [bot-auth-contract.md](bot-auth-contract.md).

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/bots` | Configured processes with runtime/desired state, restart metadata, QR flag. |
| `POST` | `/bots/{name}/start`, `/stop`, `/restart` | Process control. |
| `GET` | `/bots/{name}/logs` | Recent stdout/stderr. Query `limit` (default 100). |
| `GET` | `/bots/{name}/qr` | QR image when the bot exposes one. |
| `GET`/`PUT`/`DELETE` | `/bots/{name}/config` | Editable `bots:` entry; PUT syncs the live manager. |
| `GET` | `/bots/auth` | Bulk `BotAuthSnapshot` (AttentionBar `NeedsAuth` poll; also records HITL snapshots). |
| `GET` | `/bots/auth/state` | Scope-wide auth state. |
| `GET` | `/bots/{name}/auth` | One bot. |
| `POST` | `/bots/{name}/auth/start` | Begin the adapter login flow (`202`). |

---

## Agents and programs

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/programs` | `{ programs: [{name, title}] }` from `programs/*.md`. |
| `GET` | `/programs/{name}` | Full document + managed `## Missions (CEO)` + `.history/` snapshots. `404`/`400` (bare `*.md` names only). |
| `POST` | `/programs/{name}/revert` | Restore from a history snapshot (`{ snapshot? }`; default newest). Current content is snapshotted first. |
| `POST`/`GET` | `/agents` | Create (`201` + `Location`) / list ambient definitions. Surface-only agents are omitted from this generic list. Disabled ambient definitions stay visible so an owner can re-enable them. |
| `GET` | `/agents/health` | Crew-health snapshot. |
| `POST` | `/agents/refresh-definitions` | Reload definitions from storage. |
| `GET` | `/agents/{id}` | One definition. `ETag` for concurrency. Self-introspection allowed; inspecting another agent requires the reachable-target set. |
| `GET` | `/agents/{id}/effective-tools` | Operator-safe effective tool-policy snapshot. `?surface=chat\|realtime_voice\|tutor\|app_copilot\|thinking_map`. No full schemas or parameter-deny values. |
| `PUT`/`PATCH`/`DELETE` | `/agents/{id}` | Full replace / RFC 7386 merge patch / `204`. PUT/PATCH require `If-Match`. |
| `POST` | `/agents/{id}/set-primary` | Mark as primary personal agent. |
| `POST` | `/agents/{id}/trigger` | Manual eligible goal cycle. Disabled and surface-only agents reject before persist. |
| `POST` | `/agents/{id}/pause`, `/resume` | Pause / resume. |
| `GET`/`PUT` | `/harness/runtime` | Effective global harness state. PUT `{ "enabled": true\|false }` persists `harness.paused`; disabling clears queued starts and requests cancellation, and removes `MAGICIAN_HARNESS_PAUSED` from the live `.env` / `.env.development`. |
| `GET` | `/harness/cycles` | Recent `harness_cycle_dispatch_outcome` events plus outcome tallies. |
| `GET` | `/harness/anomalies` | Persisted harness anomalies; optional `status=open\|fix_dispatched\|resolved\|dismissed`. |

Agent discovery is a policy projection, not an execution grant. `GET /agents` keeps disabled ambient definitions visible so an owner can re-enable them; it is not model or composer authority. Product features use typed routes (Thinking Map: `surface=thinking_map`, `feature_mode=brainstorm`).

```bash
curl -s -X PATCH http://127.0.0.1:3002/api/magician/v2/agents/personal-assistant \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -H 'If-Match: "v1"' \
  -d '{"name":"My Assistant"}'
```

---

## Skills

Registered in `skills_api::configure_skills_routes`. There are no promote/demote HTTP routes. Catalog details: [skills-quickstart.md](skills-quickstart.md).

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/skills` | Installed procedure + personality-mode skills plus embedded compiled packs (`kind: compiled`, `layer: built-in`), including workspace-layer entries from the bearer scope. |
| `POST` | `/skills/install` | Operator-privileged. `X-Magician-Setup-Token`. Body `{ "source": "path:<abs>"\|"skillshub:<name>", "target": { "workspaces": ["principal/workspace"] } }`. The system-shared install tier is retired. |
| `POST` | `/skills/{name}/allow-for-agent` | Add the skill to a named agent's allow-list. Same setup-token gate. |
| `GET` | `/skills/{name}/schema` | Parsed `tool_schema.yaml` (workspace layer wins). |
| `POST` | `/skills/{name}/run` | One-shot CLI dispatch. Body `{ action?, args?, session_params?, timeout_secs? }`. Resolves `implementation.command` (special-cases `browser` → `agent-browser`), prepends per-action `argv`, shells out with `MAGICIAN_SKILL_DIR` and `<skill_dir>/bin` on `PATH`. Returns `{ prelude, argv, exit_code, success, stdout, stderr, duration_ms, parsed_json }`. Default 60s, kill-on-drop. Bypasses the LLM inner loop. |

---

## API mining

`ApiMiningApi` uses `CapabilityRegistry`, the captured-auth `SecretStore` partition, and the persisted origin-policy store. Deeper pipeline: [api-mining-pipeline.md](api-mining-pipeline.md), [api-explorer.md](api-explorer.md).

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/api-mining/registry` | Full `RegistryIndex` (`version`, `origins`, `last_rebuilt`). `404` if unconfigured. |
| `GET` | `/api-mining/noisy-origins` | Origins with ≥25 stored traces (trace events, not only promoted capabilities), plus operator decision: `{ threshold, origins: [{ origin_key, origin_url, trace_count, capability_count, decision }] }`. |
| `GET` | `/api-mining/capabilities/{origin_key}/{capability_id}` | One `ApiCapability`. `origin_key` is the filesystem-safe key (e.g. `https___api_github_com`). |
| `GET` | `/api-mining/auth-status/{origin_key}` | Non-secret metadata: `{ has_auth, is_stale, has_cookies, has_headers, has_storage }`. |
| `POST` | `/api-mining/origins/{origin_key}/refresh-auth` | Scoped Magicutor CDP capture (no LLM). `202` new / `200` reuse. `400` if the origin is not `http`/`https`. |
| `GET` | `/api-mining/origins/{origin_key}/refresh-auth/{refresh_id}` | Non-secret refresh phase. |
| `POST` | `/api-mining/replay/{origin_key}/{capability_id}` | Replay with optional overrides. |
| `GET` | `/api-mining/openapi/{origin_key}` | OpenAPI 3.0 JSON. |
| `POST` | `/api-mining/origins/{origin_key}/allow`, `/block`, `/purge`, `/block-and-purge` | Origin policy. Trace-only origins may send `{ "origin_url": "https://…" }`. |

---

## Unified Event Stream — `GET /api/magician/v3/events`

Live tail of `RuntimeTransportEvent` through the runtime broadcaster, with optional disk backfill. NDJSON (`application/x-ndjson`), one event per line. Same events as `/realtime/ws`. Persistence: [v2-websocket-events.md#event-persistence--retention](v2-websocket-events.md#event-persistence--retention).

```
GET /api/magician/v3/events?<filters>
Authorization: Bearer <workspace-bound-token>
```

Query params (optional, AND-combined):

| Param | Description |
| --- | --- |
| `execution_id`, `task_id` | When both are set, backfill reads the per-execution `events.jsonl`. Otherwise cross-scope backfill uses the per-scope log plus recent per-execution files. |
| `since` | Earliest `timestamp_ms`. Per-execution: no implicit floor. Cross-scope execution enumeration defaults to now−24h; pass `since=0` to widen. |
| `before` | Backfill rows with `timestamp_ms < before`. Live tail unaffected. |
| `limit` | Backfill cap. Default `EVENTS_RETENTION_MIN_COUNT` (2000). |
| `category` | Comma-separated `EventCategory` tokens (`pipeline,plan,tool,slot,clarification,execution,llm,agentic,hitl,agent,task,feed,observability,media,activity`). Unknown tokens are dropped. |
| `severity` | `info,warn,error,decision,attention`. |
| `user_relevant` | `true` / `false`. Omitted = all. |
| `event_type` | Substring match. |
| `agent_id` | Equality when present. |
| `search` | Substring against serialized JSON. |
| `backfill_only` | `true` closes after backfill (history snapshots). Default keeps the live tail open. |

Synthetic rows:

- `{"event_type":"__events_partial__","scanned":N,"scan_cap":M,"message":"…"}` — cross-scope backfill hit the 50k-event hard scan cap.
- `{"event_type":"__events_lagged__","skipped":N,"timestamp_ms":…,"message":"…"}` — live-tail broadcaster dropped events for this request (in-flight only).
- `{"event_type":"__transport_log_lagged__","skipped":N,"timestamp_ms":…,"message":"…"}` — on-disk writer lagged; the gap is permanent for that window.

**Backfill paths**

1. **Per-execution** (both `execution_id` and `task_id`) — reads `…/scopes/{principal}/{workspace}/tasks/{task_id}/executions/{exec_id}/events.jsonl`. No implicit `since` floor.
2. **Cross-scope** (neither, or `task_id` alone) — reads `…/scopes/{principal}/{workspace}/events.jsonl` and enumerates per-execution files newer than the effective `since`. Rows are ordered by `timestamp_ms` descending in a bounded top-`limit` accumulator (default 2000), not the 50k scan cap. The per-scope log is read only as far as its length at open. A `?task_id=` request resolves that task’s executions directly. Both legs sample client-disconnect every 256 lines.

Workspace filter is fail-closed (`event_visible_to_scope`). The live-tail task races `recv()` against the response `closed()` future so disconnects release immediately.

---

## Common workflows

### Create → message → inspect

```bash
EXECUTION_ID=$(curl -s -X POST http://127.0.0.1:3002/api/magician/v2/executions \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -d '{"title":"Diagnostics"}' | jq -r '.execution_id')

curl -s -X POST http://127.0.0.1:3002/api/magician/v2/executions/$EXECUTION_ID/message \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN" \
  -d '{"text":"Check if google.com is reachable"}'

curl -s "http://127.0.0.1:3002/api/magician/v2/executions/$EXECUTION_ID/turns?direction=Inbound" \
  -H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN"
```

### Clarifications and pauses

List pending plan questions with `GET /api/magician/v3/tasks/$TASK_ID/plan/clarifications/pending`. Submit answers and agentic/user-request/approval decisions only through `POST /api/magician/v2/hitl/{correlation_id}/respond`. Manual planner resume: `POST /api/magician/v3/tasks/$TASK_ID/plan/clarifications/resume`. Tree pause/resume uses `/executions/{id}/pause` and `/resume`, not HITL.

---

## Errors

Typical envelope (`ApiErrorEnvelope`):

```json
{ "code": "invalid_request", "error": "Human readable message", "details": {} }
```

| Status | Meaning |
| --- | --- |
| `400` | Validation (`invalid_request`). |
| `401` | Missing/invalid bearer (`WWW-Authenticate: Bearer`). |
| `403` | Scope mismatch on a HITL/control path. |
| `404` | Missing execution/resource (`resource_not_found`). Cross-scope lookups look like not found. |
| `409` | Conflict (steer queue full, HITL `already_resolved`). |
| `410` | Retired HITL respond URL. |
| `500` | Unhandled failure (`internal_error`). |

- **Pagination**: `limit` defaults to 50 (max 200 on execution/turn lists). Use `pagination.has_more`.
- **Idempotency**: most writes are not idempotent; dedupe client retries.
- **Concurrency**: the file-backed store uses per-execution locks. Pause/resume/cancel share the root-id lock.
- **Scope**: do not send `principal`/`workspace` as client authority. Defaults `anonymous`/`default` exist only for local open-mode bootstrap.
- **Streaming**: dashboards should use `/realtime/ws` or `GET /api/magician/v3/events`, not removed analysis polling (`/analyze`, `/executions/{id}/analysis`).

---

## Related references

- `magician-bin/src/main.rs` — Actix scopes (`/api/magician/v2`, `/api/magician/v3`).
- `magician-api/src/` — handlers (`web_api.rs`, `task_api_v3.rs`, `events_api.rs`, `chat_api.rs`, `meetings_api.rs`, …).
- `magician/src/magician_v2/storage/models.rs` — `ExecutionRun`, `V2Turn`, `WaitingState`.
- `magician/tests/magician_v2_api_integration_test.rs` — HTTP integration coverage.
- [Auth](auth.md), [HITL](hitl-attention.md), [V3 flow](v3-complete-flow.md), [WebSocket events](v2-websocket-events.md), [ARCHITECTURE_V2.md](../../ARCHITECTURE_V2.md).
