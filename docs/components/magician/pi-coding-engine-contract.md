<!-- Part A line ranges are the Pi 0.79.2 extraction. Runtime completion is the reviewed Pi 0.87.1 contract below. Design archive: docs/archive/plans/2026-06-14-vibedev-100x-plan.md -->

# Pi Coding-Engine Contract

Pi is the configured and UI-default VibeDev coding engine.
Siblings: [Grok](grok-coding-engine-contract.md),
[Claude Code](claude-coding-engine-contract.md),
[Antigravity](agy-coding-engine-contract.md).

Crate-relative under `magician/src/magician_v2/`: PI
`execution/coding_engine/pi.rs`, MOD `execution/coding_engine/mod.rs`, HANDLER
`execution/compiled_handlers/run_coding_task.rs`, BROADCAST
`realtime_events.rs`, EVENTS_API `magician-api/src/events_api.rs` (outside this root). Part A paths were under
`/tmp/pi-inspect/node_modules/@earendil-works/pi-coding-agent/` at extraction.

## Current runtime contract — Pi 0.87.1

`scripts/setup-pi-coding-agent.sh` pins `@earendil-works/pi-coding-agent@0.87.1`.
The adapter probes `pi --version` (5s, `PI_RPC_REQUIRED_VERSION`) and refuses
unreviewed versions before RPC. A successful `prompt` still means only that Pi
accepted the prompt. `agent_end` is **non-terminal** regardless of `willRetry`
(auto-retry, compaction retry, extension continuation, queued follow-up may
re-enter). Only `agent_settled` closes the session-level run; Magician waits
for it before final text/state/stats, shadow staging, or shutdown.

Projection recognizes `agent_settled`, `entry_appended`, `session_info_changed`,
`summarization_retry_*`, `bash_execution_update`. Settlement, thinking-level
changes, and summarization retries ride `coding.*`; session bookkeeping and
direct-bash stay in raw history. Fixtures prove `agent_end` cannot release
normal / auto-retry / compaction-retry / queued follow-up flows. A stream that
closes before `agent_settled` fails closed. Conformance requires
`agent_settled` as terminal evidence. `get_session_stats` is cumulative
(including tool/compaction/branch-summary); Magician's before/after delta is
the turn boundary and is sampled only after settlement.

Part A is the 0.79.2 wire extract. Where it names `agent_end` as turn
completion, the runtime (0.83 onward) uses `agent_settled`. The one later wire
change Magician depends on is `message_update` (0.84.0): the cumulative
`message` and `assistantMessageEvent.partial` are gone and cumulative usage is
a top-level `usage` (MOD reads it first; zero until the provider reports it).
`thinking_delta` updates stream. The Citizen extension only calls
`registerTool` with schemas, so 0.84+ extension-API breaks do not touch it.
Node floor: 22.19.0. `pi-coding-agent@0.87.1` pins its own `pi-ai` /
`pi-agent-core` / `pi-tui` via shrinkwrap.

The private OpenAI route supplies `gpt-6.1-sol` to Pi's Responses adapter with
an explicit generated `models.json` entry (1,050,000 context, the profile's
output cap up to 128,000, thinking levels low–max, and tiered rates above
272,000 input tokens), so Pi does not fall back to a generic catalog entry
with zero pricing and unsupported `off`/`minimal` levels.

Fixture: `execution/coding_engine/fixtures/pi_rpc_session_0.87.1.jsonl`
(replayed through the production parser and collector in unit tests).

---

# PART A — Pi 0.79.2 RPC Contract Reference

## A.1 Mode selection and process lifecycle

RPC is selected **only** by `--mode rpc` (`"text" | "json" | "rpc"`; no
`--rpc`/`--json`/`--event-stream` flag) → `runRpcMode`, which **never
returns**. Subscribe: `session.subscribe` → JSONL on stdout. Stdin EOF →
`shutdown()`; also `SIGTERM`(143)/`SIGHUP`(129). **Closing stdin kills the
process.** RPC does not read piped stdin as a prompt and rejects `@file` args.

## A.2 Wire framing (JSONL, LF-only)

``serializeJsonLine(value) = `${JSON.stringify(value)}\n` ``. Read splits on
`\n` only (strip trailing `\r`). Do **not** use Node `readline` — it also
breaks on U+2028/U+2029, valid inside JSON strings. Commands on **stdin**;
responses + events + extension-UI requests on **stdout**. No JSON-RPC 2.0
envelope, length-prefix, or per-line batching. Pi `{type, …}` plus optional
free-form `id`.

## A.3 Three stdout record classes (top-level `type` discriminant)

| Top-level `type` | Class | Has `id`? | Source |
|---|---|---|---|
| `"response"` | `RpcResponse` (command reply, has `command`, `success`) | yes (echoes request `id`) | `rpc-types.d.ts:148-346` |
| `"extension_ui_request"` | blocking UI request needing `extension_ui_response` | yes | `rpc-types.d.ts:347-404` |
| any other value | `AgentSessionEvent` (loop event) | **no** | `agent-session.d.ts:40-77` |

Events carry **no `id`**. Distinguish a response from an event by
`type === "response"`.

## A.4 Full event-kind table

Canonical union: `AgentSessionEvent` at `dist/core/agent-session.d.ts:40-77` =
`Exclude<AgentEvent,{type:"agent_end"}>` (forwarded verbatim by
`_handleAgentEvent`) + enriched `agent_end` + session-only events.

### Core loop events (`pi-agent-core/dist/types.d.ts:359-397`)

| `type` | Fields |
|---|---|
| `agent_start` | *(none)* |
| `turn_start` | *(none on the RPC stream — no `turnIndex`/`timestamp`)* |
| `turn_end` | `message: AgentMessage`, `toolResults: ToolResultMessage[]` |
| `message_start` | `message: AgentMessage` |
| `message_update` | `message: AgentMessage`, `assistantMessageEvent: AssistantMessageEvent` |
| `message_end` | `message: AgentMessage` |
| `tool_execution_start` | `toolCallId: string`, `toolName: string`, `args: any` |
| `tool_execution_update` | `toolCallId`, `toolName`, `args`, `partialResult: any` (accumulated, not a delta) |
| `tool_execution_end` | `toolCallId`, `toolName`, `result: any`, `isError: boolean` |

Enriched `turnIndex`/`timestamp` exist only on the extension-facing
`TurnStartEvent`/`TurnEndEvent`, never on the RPC stream. Count `turn_start`s
client-side.

### `message_update.assistantMessageEvent` (`pi-ai/dist/types.d.ts:257-310`)

Second-level discriminant `assistantMessageEvent.type`. Each variant except the
terminal two carries `partial: AssistantMessage`.

| `type` | Fields |
|---|---|
| `start` | `partial` |
| `text_start` | `contentIndex`, `partial` |
| `text_delta` | `contentIndex`, `delta: string`, `partial` |
| `text_end` | `contentIndex`, `content: string`, `partial` |
| `thinking_start` | `contentIndex`, `partial` |
| `thinking_delta` | `contentIndex`, `delta: string`, `partial` |
| `thinking_end` | `contentIndex`, `content: string`, `partial` |
| `toolcall_start` | `contentIndex`, `partial` |
| `toolcall_delta` | `contentIndex`, `delta: string` (raw arg JSON fragment), `partial` |
| `toolcall_end` | `contentIndex`, `toolCall: ToolCall`, `partial` |
| `done` | `reason: "stop"｜"length"｜"toolUse"`, `message: AssistantMessage` |
| `error` | `reason: "aborted"｜"error"`, `error: AssistantMessage` |

`AssistantMessage`: `role`, `content:(TextContent｜ThinkingContent｜ToolCall)[]`,
`api`, `provider`, `model`, `responseModel?`, `responseId?`, `diagnostics?`,
**`usage: Usage`**, `stopReason: StopReason`, `errorMessage?`, `timestamp`.
`Usage`: `input`, `output`, `cacheRead`, `cacheWrite`, `totalTokens`,
`cost:{input,output,cacheRead,cacheWrite,total}`.
`StopReason`: `"stop"｜"length"｜"toolUse"｜"error"｜"aborted"`.

### Session-only events (`agent-session.d.ts:40-77`)

| `type` | Fields |
|---|---|
| `agent_end` | `messages: AgentMessage[]`, `willRetry: boolean` |
| `queue_update` | `steering: readonly string[]`, `followUp: readonly string[]` |
| `compaction_start` | `reason: "manual"｜"threshold"｜"overflow"` |
| `compaction_end` | `reason`, `result: CompactionResult｜undefined`, `aborted: boolean`, `willRetry: boolean`, `errorMessage?` |
| `session_info_changed` | `name: string｜undefined` |
| `thinking_level_changed` | `level: ThinkingLevel` |
| `auto_retry_start` | `attempt`, `maxAttempts`, `delayMs`, `errorMessage` |
| `auto_retry_end` | `success`, `attempt`, `finalError?` |

`docs/rpc.md` also lists `extension_error`. `ThinkingLevel`:
`"off"｜"minimal"｜"low"｜"medium"｜"high"｜"xhigh"`.

## A.5 Command (request) shapes

`RpcCommand` (`rpc-types.d.ts:13-122`); optional `id` on each.
`prompt` (`images?`, `streamingBehavior?:"steer"|"followUp"` — **camelCase**),
`steer`, `follow_up` (**snake_case**), `abort`, `new_session`, `get_state`,
`set_model`, `cycle_model`, `get_available_models`, `set_thinking_level`
(`off|minimal|low|medium|high|xhigh`), `cycle_thinking_level`,
`set_steering_mode` / `set_follow_up_mode` (`all|one-at-a-time`), `compact`,
`set_auto_compaction`, `set_auto_retry`, `abort_retry`, `bash` (output not fed
to the LLM until the next `prompt`), `abort_bash`, `get_session_stats`,
`export_html`, `switch_session`, `fork`, `clone`, `get_fork_messages`,
`get_last_assistant_text`, `set_session_name`, `get_messages`, `get_commands`.

Images: `{"type":"image","data":"<base64>","mimeType":"image/png"}`.

During streaming a plain `prompt` is **rejected** unless `streamingBehavior`
is set. `steer` arrives after the current assistant turn finishes its tool
calls, before the next LLM call (no extension `/commands` via `steer`).
`follow_up` arrives only when the agent fully stops. `abort` →
`session.abort()`.

## A.6 Response shapes

Success `{id?, type:"response", command, success:true, data?}`; failure
`{id?, type:"response", command, success:false, error}`.
`get_state` → `RpcSessionState` (`model?`, `thinkingLevel`, `isStreaming`,
`isCompacting`, `steeringMode`, `followUpMode`, `sessionFile?`, `sessionId`,
`sessionName?`, `autoCompactionEnabled`, `messageCount`,
`pendingMessageCount`). `get_session_stats` → `SessionStats` (`tokens`,
`cost` cumulative USD, `contextUsage?:{tokens,contextWindow,percent}` **null
right after compaction**). Also: `get_messages` `{messages}`,
`get_last_assistant_text` `{text}`, `get_commands` `{commands}`, `bash`
`BashResult`, `compact` `CompactionResult`, `export_html` `{path}`, `fork`
`{text, cancelled}`, `new_session`/`switch_session`/`clone` `{cancelled}`.

**`prompt` is async.** `{command:"prompt", success:true}` is preflight
acceptance. Later failures arrive on the event stream. 0.79.2 turn completion
was `agent_end`; Magician (0.83 onward) waits for `agent_settled`.

## A.7 Session persistence

CLI: `--session`, `--session-id`, `--continue`/`-c`, `--resume`/`-r`, `--fork`,
`--session-dir`, `--no-session`, `--name`/`-n`. Default
`~/.pi/agent/sessions/<encoded-cwd>/`; `CURRENT_SESSION_VERSION = 3`. Over RPC
use `switch_session`/`new_session`/`fork`/`clone`. `continueRecent` is
in-process only — rehydrate at spawn with `--session-dir` plus
`--continue`/`--resume`/`--session`. Env: `PI_CODING_AGENT_DIR`,
`PI_CODING_AGENT_SESSION_DIR`; `PI_OFFLINE=1` ≙ `--offline`.

## A.8 Images & extension_ui_request

Images on `prompt`/`steer`/`follow_up` via `images[]`. Dialogs
`select`/`confirm`/`input`/`editor` work over RPC. No-ops: `custom()`,
working-message/indicator, footer/header/editor component, tools-expanded,
theme switching.

## A.9 Capability table (0.79.2)

| Capability | Status | Notes |
|---|---|---|
| Warm process / `prompt`+images / `steer` / `follow_up` / `abort` | EXISTS | A.5; loop never resolves |
| Graceful stop | EXISTS | stdin EOF → `shutdown`, or SIGTERM |
| `get_session_stats` + per-message `usage` | EXISTS | A.6; no periodic cost event |
| `contextUsage` | PARTIAL | null after compaction; inner `tokens`/`percent` also nullable |
| `turnIndex`/`timestamp` on RPC `turn_*` | ABSENT | count `turn_start` client-side |
| Top-level `error` event | ABSENT | derive from `assistantMessageEvent.error`, `stopReason`, `willRetry`, compaction/retry, or `success:false` |
| Model-change event | ABSENT | only `thinking_level_changed`; use `get_state` / `set_model` |
| `switch_session`/`new_session`/`fork`/`clone` | EXISTS | `continueRecent` is in-process only |
| Provider raw hooks / HTTP transport | ABSENT on RPC | stdin/stdout JSONL; on-stream tools are `tool_execution_*` |

Reference client `RpcClient.waitForIdle` resolved on `agent_end` in 0.79.2.
Magician waits for `agent_settled`.

---

# Magician adapter

`PiSession` keeps stdin open and runs one persistent stdout reader:
`type=="response"` → id→oneshot map; everything else →
`coding_event_from_raw`. Spawn uses `os_sandbox_command`. Resume is spawn-time
(`--session-dir` plus `--session <id>` or `--continue`); an already-warm
process can `switch_session`. There is **no** cross-invocation warm pool:
each `run_coding_task` spawns, waits for `agent_settled`, then
`shutdown_graceful` (stdin EOF, bounded `terminate_child`, `kill_on_drop`).

`PiTurnOptions` (Pi-only): `session_name`, `session_dir`, `resume_recent`,
`resume_session_id`, `provider`, `model`, `thinking_level`, `extension_paths`,
`append_system_prompt`, `images`. `--session` beats `--continue`; both need
`session_dir` (`scope_root/coding_engine/pi_sessions`). Handle verbs: `prompt`
(camelCase `streamingBehavior`), `steer`, `follow_up`, `abort`,
`set_thinking_level` (best-effort before the turn), `get_session_stats`. A
plain `prompt` during streaming is a typed error.

Continuation is engine-bound. Stage 5 picks Pi or Codex from the journaled
engine; Codex binaries come from discovery, never tool args. Native thread
ids stay on `CodingContinuationRef`. Codex `resume_thread_id` binds through
`resolve_previous_chain_continuation` + `codex_lifecycle::resume_or_fresh`
(`scope_binding_digest`, `project_binding_digest`, `root_task_id`,
`generation`). Cross-engine in the same execution starts fresh
(`CrossEngine`); Pi→Codex→Pi starts a fresh Pi session. Coding jobs are
`RetrySafety::Reattachable` via the effect ledger (never `Refire`).
`continuation_lost` starts fresh.

`run_coding_task` / `apply_code_proposal` / `run_project_checks` must be
registered in `build_compiled_registry` or dispatch fails
``not a compiled pack``.

## Event projection and SSE

`CodingEngineEvent` fields: `thinking_delta`, `assistant_event_type`, `usage`,
`cost_total`, `stop_reason`, `tool_result_is_error`, `will_retry`,
`error_message`, `raw`. `CodingUsage` mirrors A.4. Usage is read from
`raw["message"]["usage"]`, then the top-level `raw["usage"]` (Pi 0.84+
`message_update`), then
`raw["assistantMessageEvent"]["partial"|"message"|"error"]["usage"]`
(first present wins; older builds).

`CodingEventEmitter` stamps `engine` / `shadow_workspace_id` / `task_id` /
`execution_id` / `chat_turn_id` and `emit_named`s onto the existing rail →
`GET /api/magician/v3/events` (NDJSON; `event_type=coding.` substring filter)
and chat fan-out by `task_id`. Its backfill phase replays scope JSONL
(`backfill_only=true` stops there). `reader_loop` tees to
`tracing` target `coding_engine::pi` (no `raw` or tool bodies). Turn index is
synthetic. `coding.failed` is derived.

| Adapter kind | Rail |
|---|---|
| `AgentStart` / `AgentEnd` / `AgentSettled` | `coding.agent_started` / `coding.agent_ended` (`will_retry`) / `coding.agent_settled` |
| `TurnStart` / `TurnEnd` | `coding.turn.started` / `coding.turn.finished` `{usage, cost_total, stop_reason}` |
| `MessageUpdate` | `coding.message` / `coding.thinking` — **verbatim** whitespace, `truncate_chars` 2000, never `trim` / `trim_for_event` |
| `MessageEnd` | `coding.message.finished`; `stop_reason=="error"` also `coding.failed {stage:"pi_message"}` |
| `ToolExecutionStart` / `Update` / `End` | `coding.tool.started` (redacted args) / `progress` / `finished` (redacted result) |
| `QueueUpdate` | `coding.queue {steering_len, follow_up_len}` |
| `CompactionStart` / `End` | `coding.compaction.started` / `finished {will_retry, error_message}` |
| `AutoRetryStart` / `End` | `coding.retry.started` / `finished` |
| `SummarizationRetry*` | `coding.summarization.retry_*` |
| `ThinkingLevelChanged` | `coding.thinking_level.changed` |
| `ExtensionError` | `coding.failed {stage:"pi_extension"}` |
| `Response`, `MessageStart`, `EntryAppended`, `SessionInfoChanged`, `BashExecutionUpdate` | dropped from projection; kept in `event.raw` |

Every projected event is also teed to per-execution `coding_events.jsonl`
(immune to the transport-log trim of ~2000 events / 24h).
`DurableCodingWriter` coalesces `coding.message`/`coding.thinking` deltas at
the next structural event, keyed by the first delta's `sequence`+`ts`, flushed
**before** that event so `coding.turn.started` order holds. Shape
`{event_type, data, timestamp_ms}`.
`GET /vibedev/runs/{task_id}/coding-events` (NDJSON, 64 MiB ceiling);
cockpit hydrates durable-first, else trimmed `/v3/events`.

## Control plane

`CodingControlRegistry` keys `scoped_control_key(principal, workspace, id)`.
`run_turn` registers `task_id` / `execution_id` / `shadow_workspace_id` before
streaming and unregisters on settle. Empty `control_keys` ⇒ not steerable.
`CodingControlAction`: `Steer`, `FollowUp`, `Stop`.
`POST /api/magician/v2/vibedev/runs/{run_id}/control`
`{ action: "steer"|"follow_up"|"stop", message? }`. `409` if no live run.
Stop on `Ok(false)` falls back to `cancel_execution_by_id`
(`active_root_execution_id` / `latest_root_execution_id`, else `run_id` as
execution id). steer/follow_up keep the plain 409.

Cancel reaches `run_turn` via task-local `EXECUTION_CANCEL_TOKEN` →
`request.cancel_token`. `execute_action` gives `run_coding_task` a 3s window
to unregister the handle then drain Pi; every other pack drops instantly.

`try_synthesize_stuck_auto_yield` also counts identical-action churn
(`iteration_is_repeat_churn`) at `NO_PROGRESS_REPEAT_AUTO_YIELD_THRESHOLD`
(3 → 4 identical calls).

## Shadow workspace, staging, diffs

Pi never edits the real repo. `sync_persistent_workspace` syncs a per-repo
shadow (`persistent_shadow_key` blake3): copy tracked files,
`prune_shadow_strays`, never touch `node_modules`/`target`/`.venv`. One
active run per repo.

`stage_result` defaults true. After the turn, `attach_staged_coding_proposal`
byte-diffs shadow vs real (`read_bytes_bounded_local`, compare first).
Unchanged files (incl. binary) skip with no decode; a changed binary is
omitted from the text patch (counted + warn) rather than aborting. Per-file
size cap still errors. Pi must not `git add`/commit/hand back a diff —
`append_coding_context` says so. Ignored dirs also include home-level
`.config`/`.local`/`.npm`/`.cargo`/`.rustup`/`.gradle`/`.m2`/`.docker`/`.kube`/
`.aws`/`.azure`/`.gnupg`/`.ssh`/`.mozilla` (scope-home projects).

Build tasks wrap the raw request in `<<<VIBEDEV_USER_PROMPT`. Delegated
coding workers call `run_coding_task` first so `coding.*` streams immediately.

## Preview, checks, isolation

`DevServerSessionManager` (keyed by `project_id`) spawns the project dev
command in the shadow, scrapes the ready URL (`0.0.0.0`/`127.x`/`[::1]` →
`localhost`), idle-evicts after 30m. Preview proxy: HTTP + WS-upgrade,
upstream `Host` → `localhost`.
`GET/POST /vibedev/projects/{id}/preview[/start|/stop]` and
`…/preview/proxy/{tail}` (`web::route()`, including WS). Vite `server.hmr.path`
is injected. `text/html` gets a dormant click-to-edit overlay (`postMessage`).

`GET /vibedev/projects/{id}/info` → `{ project_kind, previewable, checks[] }`
(`detect_project_kind` Rust/Node/Python/Static/Unknown).
`POST /vibedev/projects/{id}/check` (`{kind?}`, 240s, `CI=1`/`NO_COLOR=1`) →
`{ kind, command, ok, exit_code, timed_out, output_tail }[]`.

Isolation: dugite missing cwd → sandbox + `GIT_CEILING_DIRECTORIES=workdirs_root`;
`resolve_coding_repo_binding` rejects live-repo `repo_path`;
`validate_shell_action_hard` rejects live-repo `working_dir`;
`os_sandbox_command` (macOS `sandbox-exec` / Linux `bwrap`) makes live repo
source read-only and `magician_data_v3` writable. Default-on after launcher
self-test; `MAGICIAN_CODING_OS_SANDBOX=0` kills it. Fail-open if the launcher
is missing; fail-closed if a wrapped launch then rejects. Codex/Grok/Claude/Agy
require `require_outer_fence()`.

## M1

`apply_code_proposal` applies the run's own staged proposal (idempotent
`already_applied` when not Pending). Shadow re-sync at the **start of every
turn** wipes an unapplied proposal. `run_project_checks` wraps the shadow
runner. Both granted to senior/principal/frontend/junior-* (system template
**and** `scopes/anonymous/default`); `notify_owner` to `engineering-manager`.
Autopilot: branch-first (never main), coding → apply → checks → fix (cap 12),
commit, `notify_owner`. Autopilot **or** an agent that already has
`apply_code_proposal` does **not** park on that proposal's `diff_approval`.
Accept-in-scope skips permission HITL for in-tree file tools, not
`diff_approval`. Attended Build still prompts.

## M5

`screenshot_preview` (`project_id`, optional `full_page`) captures the live
preview headlessly (60s; soft `{ok:false}` if none) into a `#vibedev`
attachment on synthetic session `vibedev-shots-<project_id>` (newest-12).
IMAGE records become RPC `images[]` on `PiTurnOptions.images` — **not** a
shadow path. Granted to the five engineer agents. Cockpit visual self-correct
is opt-in, hard cap 3.

## Citizen API

Extension `magician/assets/pi-extensions/magician-citizen.ts` (embedded,
outside the shadow). `CitizenTokenRegistry` mints a `CitizenGrant` per turn
and **revokes it on settle**. Bearer token **is** the scope.
`POST /api/magician/v2/vibedev/citizen/preview_url` →
`dev_server_manager().status(project).local_url` (loopback).
`POST /vibedev/citizen/secret` → scoped broker (`magician_citizen:dev_server_env`)
→ `set_secret_env`; response `{ok, env_var, requires_restart}` — never the
value. Project: explicit arg → grant `project_id` →
`active_root_task_id` match. `magician_code_knowledge` is live;
`magician_lsp` is unbuilt.
m6 archive.

## HITL and terminalization

`diff_approval` pauses the execution. `reconcile_stale_active_root_execution`
finalizes an orphan (runtime gone + no terminal event + non-terminal task +
**no** `resumable_pause_exists`) as `failed`
(`ExecutionOutcomeSnapshot::orphaned_execution`). A terminal `WaitingState`
emits `HitlResolved { source: "diff_approval" }` (`cancelled` vs `superseded`)
before clearing pauses. Reject/apply with no live pause but
`body.execution_id` set: drain `full_pause_store().remove_for_execution`, then
`ExecutionFailed` (reject) or guarded `ExecutionStart`→`ExecutionComplete`
(apply).

## Coding budgets

Duration is a backstop. Resolved **once** into `ResolvedCodingBudgets`.

| Mechanism | Config | Catches |
|---|---|---|
| No-progress detector | `coding.no_progress.*` | wedged run (silence, not duration) |
| Cost ceiling | `MAGICIAN_AGENTIC_MAX_COST_USD` + token meter | runaway spend |
| Loop detector | executor `LoopDetector` | semantic repetition |
| Turn wall clock | `coding.turn_timeout_secs` (8 h, `0` = none) | a bug in the above |
| Whole-task ceiling | `coding.task_budget_secs` (**`0` = none**) | opt-in hard cap |

Defaults: `turn_timeout_secs: 28800`, `task_budget_secs: 0`,
`verification_reserve_secs: 1200`, `no_progress.enabled: true`,
`model_idle_secs: 900`, `tool_idle_secs: 1500`, `tool_max_secs: 3000`,
`compaction_max_secs: 900`, `summarization_max_secs: 900`,
`retry_grace_secs: 120`. Legacy `timeout_secs` is still accepted; conflicting
values are rejected. `0` must never clamp to `1`
(`budgets::clamp_turn_timeout_secs`). Pack YAML back-fill stamps
`PACK_DEFAULT_TIMEOUT_MARKER`; the handler ignores it.

`coding_execution_max_duration()` ignores the generic 40-minute agentic
default; with no task ceiling it returns **`None`**. Explicit
`MAGICIAN_AGENTIC_MAX_DURATION_SECS` is honoured (`0` = unbounded). Used by
`run_coding_task` and by `execute_agentically` when `execution_is_coding`.
`task_active_max` is `Option`; `remaining` is absent, not zero, when unbounded.

`ProgressWatchdog` in `collect_events_until_boundary`. Identical/empty events
reset nothing (fingerprint ring of 8). `coding.no_progress.enabled: false`
disarms it.

| Phase | Advanced by | Bound |
|---|---|---|
| model | substantive text/thinking delta | `model_idle_secs` |
| tool | real output or a tool state change | `tool_idle_secs` **and** `tool_max_secs` |
| compaction | — | `compaction_max_secs` |
| retry wait | — | declared `delayMs` + `retry_grace_secs` |
| summarization retry | — | `summarization_max_secs`, or a longer declared delay |

Tool phase holds until the **last** in-flight `toolCallId` ends; max-runtime
anchors on the **first** of the batch.

`CodingTerminationReason`: `TurnTimeout`, `TaskBudget`,
`NoProgress { phase, elapsed, last_substantive_event }`, `OwnerCancelled`,
`ParentDeadline`, `CostBudget`, `ServiceShutdown`. First writer wins.
Payloads carry `termination` + `budget_stop`.

`CodingTaskBudgetLedger` at `<task_dir>/coding_budget_ledger.json`, keyed on
the **root** task. Active time is the **union** of intervals. Diff-approval
time is outside every interval. Declared backoff is deducted. A crash mid-turn
seals at one turn on reload. The ledger stops work only when a ceiling is
set (`verification_reserve_secs` held back). Corrupt ledger → in-memory +
warn.

Escape hatches if the watchdog misfires: `coding.no_progress.enabled: false`,
`coding.no_progress.tool_idle_secs`.

## plan_only

Discuss / plan run: `plan` tag (`current_is_plan`) **or** `plan_only` arg →
`stage_result=false`. No byte-diff, so no `diff_approval`. Plan text goes to
temp `plan.md` (existing tool-output capture). `coding.completed` carries
`plan_run` and `assistant_text` via `truncate_chars` (~20k). Build runs keep
the 500-char `trim_for_event` preview. `coding.plan_artifact_not_captured` if
empty/write-fail. See [agentic-loop-termination.md](agentic-loop-termination.md).

## Checkpoint rewind

Minted on every checks-passing apply (`mint_checkpoint_on_green`): `git_sha`
on `refs/vibedev/checkpoints/<id>` (throwaway `GIT_INDEX_FILE`), `repo_path`,
native `source_session_id`.
`POST /vibedev/runs/{task_id}/checkpoints/{checkpoint_id}/revert` snapshots
current worktree (`undo_ref`) then `git restore --source=<sha> --worktree -- .`
(does not delete later files; never touches branch/index; 400 if no git
anchor). Writes `set_pending_engine_resume` under
`<scope_root>/checkpoints/.pending_pi_resume/<task_id>`; the next turn binds
the native id **only when** `engine` matches (`pi` / `codex_app_server` /
`grok_acp`). Matching Pi → `resume_session_id` → `--session <id>`. Cockpit:
hover ⟲ on git-anchored nodes, behind a confirm.

## Model, vision, `/llm`

Profile order: tool `coding_profile`/`profile` → agent's
`llm_routing.coding_profile` → `coding.default_profile`. Catalogue: GPT-6
Sol (`coding-balanced`, 64k output, the default; `coding-premium`), GPT-6
Astra (`coding-astra`), Claude Opus 5 (`coding-opus5`), Claude Opus 5.5
(`coding-opus55`), Claude Fable 5.1 (`coding-fable`), Luna, Grok 4.7
(`coding-grok47`), DeepSeek V4.1 Flash (`coding-deepseek-v4.1-flash`),
MiniMax M3.
V4.1 Flash accepts screenshots and thinking on one model. Kimi K3 is not
advertised.

The profile's provider and model go to Pi verbatim (`--provider`,
`--model`), and its `api_key_env` is forwarded into Pi's curated env (Pi
reads `XAI_API_KEY` for `xai`). The coding turn's billed cost is Pi's own
figure (the stats delta), so it is only as right as Pi's catalogue entry.
0.87.1 lists every shipped coding model natively. A model newer than the installed catalogue still runs: Pi clones the
provider's default entry under the requested id (warning "Using custom model
id") and bills at that entry's rates.

`/llm` records only `LLMResponseReceived`. `run_turn` samples stats before
the turn and on every terminal path (error/timeout/cancel: 5s bound) and
writes the **delta** (`CodingTurnUsage`) to `usage_capture`.
`emit_llm_call` maps it onto the typed path (`operation`/`response_kind` =
`"coding"`). Skip zero-spend and skip a resumed session whose `before` sample
failed. One row per run (call-count undercounts; cost/tokens exact). Cockpit
`% ctx` still reads full `CodingSessionStats`.
`CodingContextUsage.tokens`/`percent` are `Option` (Pi sends JSON `null`
after compaction / before first LLM response). `percent` is 0–100, unscaled.

## Conformance

`pi::tests::pi_rpc_conformance_when_enabled` — warm `pi --mode rpc`, prompt,
intermediate `agent_end`, terminal `agent_settled`, stats + last text.

```bash
make setup-pi-coding-agent
MAGICIAN_PI_CONFORMANCE=1 cargo test -p magician --lib pi_rpc_conformance -- --nocapture
```

No-op without the env var.

Not supported: cross-request warm pool, periodic cost events, mid-session
`set_model`, binary files in proposals, `magician_lsp`; one shadow per repo.
