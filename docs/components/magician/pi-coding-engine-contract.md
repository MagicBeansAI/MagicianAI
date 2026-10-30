<!-- Part A line ranges are the Pi 0.79.2 extraction. Runtime completion is the reviewed Pi 1.3.0 contract below. Design archive: docs/archive/plans/2026-06-14-vibedev-100x-plan.md -->

# Pi Coding-Engine Contract

Pi is the configured and UI-default VibeDev coding engine. Siblings:
[Grok](grok-coding-engine-contract.md),
[Claude Code](claude-coding-engine-contract.md),
[Antigravity](agy-coding-engine-contract.md).

Crate-relative under `magician/src/magician_v2/`: PI
`execution/coding_engine/pi.rs`, MOD `execution/coding_engine/mod.rs`, HANDLER
`execution/compiled_handlers/run_coding_task.rs`, BROADCAST
`realtime_events.rs`; EVENTS_API `magician-api/src/events_api.rs` (outside this
root). Part A paths were under
`/tmp/pi-inspect/node_modules/@earendil-works/pi-coding-agent/` at extraction.

## Current runtime contract — Pi 1.3.0

`scripts/setup-pi-coding-agent.sh` pins `@earendil-works/pi-coding-agent@1.3.0`
(Node floor 22.19.0; no shrinkwrap since 1.0.1, only Pi's own packages are
pinned exactly). The adapter checks `pi --version` (5s, `PI_RPC_REQUIRED_VERSION`) and refuses
unreviewed versions before RPC.

- **Built-in extensions (0.99+: MCP client, codemode, tool search).** Every
  governed launch passes `--no-mcp` (else the operator's `mcp.json` servers
  connect); Plane and `harness-pi` launches also pass `--no-extensions`, which
  disables the built-ins too. Only unrestricted Plane runs keep them.
- **Arguments.** `--provider` requires `--model` (1.0), so the adapter sends
  the provider only with a model, and maps magicllm `gemini` to Pi's `google`.
- **Additive fields.** `prompt`/`steer`/`follow_up` responses carry
  `data.disposition` (0.99); `agent_settled` carries `aborted` (1.1); nested
  tool events carry `parentToolCallId`. The parser ignores them.
- **Print mode.** `pi -p` exits 1 when the answer is cut off at the output
  limit (1.2); `harness-pi` treats that as a failed call.

- **Settlement.** A successful `prompt` is only acceptance. `agent_end` is
  **non-terminal** whatever `willRetry` says (retries, extension continuation
  or a queued follow-up may re-enter); only `agent_settled` closes the run.
  Magician waits for it before final text/state/stats, staging or shutdown; a
  stream that closes first fails closed. Part A's `agent_end` completion is
  0.79.2; 0.83 onward uses `agent_settled`.
- **Projection** adds `agent_settled`, `entry_appended`,
  `session_info_changed`, `summarization_retry_*`, `bash_execution_update`:
  settlement, thinking-level and summarization-retry events ride `coding.*`;
  session bookkeeping and direct-bash stay raw.
- **Stats.** `get_session_stats` is cumulative (incl. tool/compaction/
  branch-summary); turn usage is the before/after delta, sampled after
  settlement.
- **`message_update` (0.84.0):** cumulative `message` and
  `assistantMessageEvent.partial` are gone; cumulative usage is top-level
  `usage` (MOD reads it first; zero until reported); `thinking_delta` updates
  stream. The Citizen extension only calls `registerTool`, so 0.84+
  extension-API breaks miss it.
- **`gpt-6.1-sol`** (private OpenAI route) has a generated `models.json` entry
  for Pi's Responses adapter (1,050,000 context, profile output cap ≤128,000,
  thinking low–max, tiered rates above 272,000 input tokens) instead of a
  generic entry with zero pricing and unsupported `off`/`minimal` levels.
- **`gpt-6.1-terra`** uses the same physical limits with `off` mapped to `none`; private Pi routes take base and long-context rates from Magician's pricing table.

Fixture `execution/coding_engine/fixtures/pi_rpc_session_1.3.0.jsonl` replays
through the production parser and collector, proving `agent_end` cannot
release normal / auto-retry / compaction-retry / queued follow-up flows.

---

# PART A — Pi 0.79.2 RPC Contract Reference

## A.1 Mode selection and process lifecycle

RPC is selected **only** by `--mode rpc` (`"text" | "json" | "rpc"`; no
`--rpc`/`--json`/`--event-stream`) → `runRpcMode`, which **never returns**.
`session.subscribe` → JSONL on stdout. Stdin EOF → `shutdown()`; also
`SIGTERM`(143)/`SIGHUP`(129): **closing stdin kills the process.** RPC does not
read piped stdin as a prompt and rejects `@file` args.

## A.2 Wire framing (JSONL, LF-only)

``serializeJsonLine(value) = `${JSON.stringify(value)}\n` ``. Split on `\n`
only (strip trailing `\r`); not Node `readline`, which also breaks on
U+2028/U+2029 (valid inside JSON strings). Commands on **stdin**; responses,
events and extension-UI requests on **stdout**. No JSON-RPC 2.0 envelope,
length prefix or batching: Pi `{type, …}` plus optional free-form `id`.

## A.3 Three stdout record classes (top-level `type` discriminant)

| Top-level `type` | Class | Has `id`? | Source |
|---|---|---|---|
| `"response"` | `RpcResponse` (has `command`, `success`) | yes (echoes request) | `rpc-types.d.ts:148-346` |
| `"extension_ui_request"` | blocking; needs `extension_ui_response` | yes | `rpc-types.d.ts:347-404` |
| any other value | `AgentSessionEvent` | **no** | `agent-session.d.ts:40-77` |

Tell a response from an event by `type === "response"`.

## A.4 Full event-kind table

`AgentSessionEvent` (`dist/core/agent-session.d.ts:40-77`) =
`Exclude<AgentEvent,{type:"agent_end"}>` (forwarded verbatim by
`_handleAgentEvent`) + enriched `agent_end` + session-only events.

### Core loop events (`pi-agent-core/dist/types.d.ts:359-397`)

| `type` | Fields |
|---|---|
| `agent_start` | *(none)* |
| `turn_start` | *(none on RPC — no `turnIndex`/`timestamp`)* |
| `turn_end` | `message: AgentMessage`, `toolResults: ToolResultMessage[]` |
| `message_start` | `message: AgentMessage` |
| `message_update` | `message: AgentMessage`, `assistantMessageEvent: AssistantMessageEvent` |
| `message_end` | `message: AgentMessage` |
| `tool_execution_start` | `toolCallId: string`, `toolName: string`, `args: any` |
| `tool_execution_update` | `toolCallId`, `toolName`, `args`, `partialResult: any` (accumulated) |
| `tool_execution_end` | `toolCallId`, `toolName`, `result: any`, `isError: boolean` |

`turnIndex`/`timestamp` exist only on extension-facing
`TurnStartEvent`/`TurnEndEvent`; count `turn_start`s client-side.

### `message_update.assistantMessageEvent` (`pi-ai/dist/types.d.ts:257-310`)

Discriminant `assistantMessageEvent.type`. Every variant except `done`/`error`
also carries `partial: AssistantMessage`.

| `type` | Fields |
|---|---|
| `start` | `partial` |
| `text_start` | `contentIndex` |
| `text_delta` | `contentIndex`, `delta: string` |
| `text_end` | `contentIndex`, `content: string` |
| `thinking_start` | `contentIndex` |
| `thinking_delta` | `contentIndex`, `delta: string` |
| `thinking_end` | `contentIndex`, `content: string` |
| `toolcall_start` | `contentIndex` |
| `toolcall_delta` | `contentIndex`, `delta: string` (raw arg JSON fragment) |
| `toolcall_end` | `contentIndex`, `toolCall: ToolCall` |
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

`RpcCommand` (`rpc-types.d.ts:13-122`), optional `id` on each: `prompt`
(`images?`, `streamingBehavior?:"steer"|"followUp"` — **camelCase**), `steer`,
`follow_up` (**snake_case**), `abort` (→ `session.abort()`), `new_session`,
`get_state`, `set_model`, `cycle_model`, `get_available_models`,
`set_thinking_level` (`off|minimal|low|medium|high|xhigh`),
`cycle_thinking_level`, `set_steering_mode` / `set_follow_up_mode`
(`all|one-at-a-time`), `compact`, `set_auto_compaction`, `set_auto_retry`,
`abort_retry`, `bash` (output reaches the LLM only with the next `prompt`),
`abort_bash`, `get_session_stats`, `export_html`, `switch_session`, `fork`,
`clone`, `get_fork_messages`, `get_last_assistant_text`, `set_session_name`,
`get_messages`, `get_commands`. Images:
`{"type":"image","data":"<base64>","mimeType":"image/png"}`.

While streaming, a plain `prompt` is **rejected** unless `streamingBehavior` is
set. `steer` lands after the current assistant turn's tool calls, before the
next LLM call (no extension `/commands`); `follow_up` only when the agent fully
stops.

## A.6 Response shapes

Success `{id?, type:"response", command, success:true, data?}`; failure
`{id?, type:"response", command, success:false, error}`. `get_state` →
`RpcSessionState` (`model?`, `thinkingLevel`, `isStreaming`, `isCompacting`,
`steeringMode`, `followUpMode`, `sessionFile?`, `sessionId`, `sessionName?`,
`autoCompactionEnabled`, `messageCount`, `pendingMessageCount`).
`get_session_stats` → `SessionStats` (`tokens`, `cost` cumulative USD,
`contextUsage?:{tokens,contextWindow,percent}` **null right after
compaction**). `get_messages` `{messages}`, `get_last_assistant_text` `{text}`,
`get_commands` `{commands}`, `bash` `BashResult`, `compact` `CompactionResult`,
`export_html` `{path}`, `fork` `{text, cancelled}`,
`new_session`/`switch_session`/`clone` `{cancelled}`.

**`prompt` is async:** `{command:"prompt", success:true}` is preflight
acceptance; later failures arrive on the event stream.

## A.7 Session persistence

CLI: `--session`, `--session-id`, `--continue`/`-c`, `--resume`/`-r`, `--fork`,
`--session-dir`, `--no-session`, `--name`/`-n`. Default
`~/.pi/agent/sessions/<encoded-cwd>/`; `CURRENT_SESSION_VERSION = 3`. Over RPC:
`switch_session`/`new_session`/`fork`/`clone`. `continueRecent` is in-process
only, so rehydrate at spawn with `--session-dir` plus
`--continue`/`--resume`/`--session`. Env: `PI_CODING_AGENT_DIR`,
`PI_CODING_AGENT_SESSION_DIR`; `PI_OFFLINE=1` ≙ `--offline`.

## A.8 Images & extension_ui_request

Images ride `images[]` on `prompt`/`steer`/`follow_up`. Dialogs
`select`/`confirm`/`input`/`editor` work over RPC; `custom()`,
working-message/indicator, footer/header/editor component, tools-expanded and
theme switching are no-ops.

## A.9 Capability table (0.79.2)

| Capability | Status | Notes |
|---|---|---|
| Warm process / `prompt`+images / `steer` / `follow_up` / `abort` | EXISTS | A.5; loop never resolves |
| Graceful stop | EXISTS | stdin EOF → `shutdown`, or SIGTERM |
| `get_session_stats` + per-message `usage` | EXISTS | no periodic cost event |
| `contextUsage` | PARTIAL | null after compaction; `tokens`/`percent` nullable |
| `turnIndex`/`timestamp` on RPC `turn_*` | ABSENT | count `turn_start` |
| Top-level `error` event | ABSENT | derive from `assistantMessageEvent.error`, `stopReason`, `willRetry`, compaction/retry, `success:false` |
| Model-change event | ABSENT | `thinking_level_changed` only; `get_state` / `set_model` |
| `switch_session`/`new_session`/`fork`/`clone` | EXISTS | `continueRecent` in-process only |
| Provider raw hooks / HTTP transport | ABSENT on RPC | stdin/stdout JSONL; tools surface as `tool_execution_*` |

Reference `RpcClient.waitForIdle` resolved on `agent_end` in 0.79.2; Magician
waits for `agent_settled`.

---

# Magician adapter

`PiSession` keeps stdin open with one persistent stdout reader:
`type=="response"` → id→oneshot map, everything else → `coding_event_from_raw`.
Spawn uses `os_sandbox_command`. There is **no** cross-invocation warm pool:
each `run_coding_task` spawns, waits for `agent_settled`, then
`shutdown_graceful` (stdin EOF, bounded `terminate_child`, `kill_on_drop`).
Resume is spawn-time (`--session-dir` plus `--session <id>` or `--continue`;
`--session` wins); a warm process can `switch_session`.

`PiTurnOptions` (Pi-only): `session_name`, `session_dir`
(`scope_root/coding_engine/pi_sessions`, required by both resume flags),
`resume_recent`, `resume_session_id`, `provider`, `model`, `thinking_level`,
`extension_paths`, `append_system_prompt`, `images`. Handle verbs: `prompt`
(camelCase `streamingBehavior`; plain during streaming is a typed error),
`steer`, `follow_up`, `abort`, `set_thinking_level` (before the turn; a failure
is logged and the turn runs at Pi's default level), `get_session_stats`.

Continuation is engine-bound: Stage 5 picks Pi or Codex from the journaled
engine (Codex binaries from discovery, never tool args); native thread ids stay
on `CodingContinuationRef`. Codex `resume_thread_id` binds through
`resolve_previous_chain_continuation` + `codex_lifecycle::resume_or_fresh`
(`scope_binding_digest`, `project_binding_digest`, `root_task_id`,
`generation`). Cross-engine in one execution (`CrossEngine`, incl.
Pi→Codex→Pi) and `continuation_lost` start fresh. Coding jobs are
`RetrySafety::Reattachable` via the effect ledger (never `Refire`).

`run_coding_task` / `apply_code_proposal` / `run_project_checks` must be in
`build_compiled_registry` or dispatch fails ``not a compiled pack``.

## Event projection and SSE

`CodingEngineEvent` fields: `thinking_delta`, `assistant_event_type`, `usage`,
`cost_total`, `stop_reason`, `tool_result_is_error`, `will_retry`,
`error_message`, `raw`. `CodingUsage` mirrors A.4; first present wins:
`raw["message"]["usage"]`, top-level `raw["usage"]` (Pi 0.84+
`message_update`), then
`raw["assistantMessageEvent"]["partial"|"message"|"error"]["usage"]` (older
builds).

`CodingEventEmitter` stamps `engine` / `shadow_workspace_id` / `task_id` /
`execution_id` / `chat_turn_id` and `emit_named`s onto the existing rail →
`GET /api/magician/v3/events` (NDJSON; `event_type=coding.` substring filter)
and chat fan-out by `task_id`. Backfill replays scope JSONL
(`backfill_only=true` stops there). `reader_loop` tees to `tracing` target
`coding_engine::pi` (no `raw` or tool bodies). Turn index is synthetic;
`coding.failed` is derived.

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
| `Response`, `MessageStart`, `EntryAppended`, `SessionInfoChanged`, `BashExecutionUpdate` | not projected; kept in `event.raw` |

Every projected event is also teed to per-execution `coding_events.jsonl`
(`{event_type, data, timestamp_ms}`), immune to the transport-log trim (~2000
events / 24h). `DurableCodingWriter` coalesces `coding.message`/`coding.thinking`
deltas, keyed by the first delta's `sequence`+`ts`, and flushes them **before**
the next structural event so `coding.turn.started` order holds.
`GET /vibedev/runs/{task_id}/coding-events` (NDJSON, 64 MiB ceiling); the
cockpit hydrates durable-first, else trimmed `/v3/events`.

## Control plane

`CodingControlRegistry` keys `scoped_control_key(principal, workspace, id)`;
`run_turn` registers `task_id` / `execution_id` / `shadow_workspace_id` before
streaming and unregisters on settle (empty `control_keys` ⇒ not steerable).
`CodingControlAction`: `Steer`, `FollowUp`, `Stop`.
`POST /api/magician/v2/vibedev/runs/{run_id}/control`
`{ action: "steer"|"follow_up"|"stop", message? }` → `409` if no live run. Stop
on `Ok(false)` falls back to `cancel_execution_by_id`
(`active_root_execution_id` / `latest_root_execution_id`, else `run_id`);
steer/follow_up keep the plain 409.

Cancel reaches `run_turn` via task-local `EXECUTION_CANCEL_TOKEN` →
`request.cancel_token`. `execute_action` gives `run_coding_task` 3s to
unregister the handle and drain Pi; every other pack drops instantly.
`try_synthesize_stuck_auto_yield` also counts identical-action churn
(`iteration_is_repeat_churn`) at `NO_PROGRESS_REPEAT_AUTO_YIELD_THRESHOLD`
(3 → 4 identical calls).

## Shadow workspace, staging, diffs

Pi never edits the real repo: `sync_persistent_workspace` syncs a per-repo
shadow (`persistent_shadow_key` blake3; one active run per repo) by copying
tracked files and `prune_shadow_strays`, never touching
`node_modules`/`target`/`.venv` or home-level
`.config`/`.local`/`.npm`/`.cargo`/`.rustup`/`.gradle`/`.m2`/`.docker`/`.kube`/
`.aws`/`.azure`/`.gnupg`/`.ssh`/`.mozilla` (scope-home projects).

`stage_result` defaults true: after the turn `attach_staged_coding_proposal`
byte-diffs shadow vs real (`read_bytes_bounded_local`, compare first).
Unchanged files skip undecoded; a changed binary is left out of the text patch
(counted + warn); the per-file size cap still errors. `append_coding_context`
tells Pi not to `git add`/commit/hand back a diff. Build tasks wrap the raw
request in `<<<VIBEDEV_USER_PROMPT`; delegated coding workers call
`run_coding_task` first so `coding.*` streams immediately.

## Preview, checks, isolation

`DevServerSessionManager` (per `project_id`) runs the project dev command in
the shadow, scrapes the ready URL (`0.0.0.0`/`127.x`/`[::1]` → `localhost`) and
idle-evicts after 30m. `GET/POST /vibedev/projects/{id}/preview[/start|/stop]`
and `…/preview/proxy/{tail}` (`web::route()`, HTTP + WS upgrade, upstream
`Host` → `localhost`); Vite `server.hmr.path` is injected, and `text/html` gets
a dormant click-to-edit overlay (`postMessage`).

`GET /vibedev/projects/{id}/info` → `{ project_kind, previewable, checks[] }`
(`detect_project_kind` Rust/Node/Python/Static/Unknown).
`POST /vibedev/projects/{id}/check` (`{kind?}`, 240s, `CI=1`/`NO_COLOR=1`) →
`{ kind, command, ok, exit_code, timed_out, output_tail }[]`.

Isolation ([coding-engine-isolation.md](coding-engine-isolation.md)): dugite
missing cwd → sandbox + `GIT_CEILING_DIRECTORIES=workdirs_root`; a live-repo
`repo_path` / `working_dir` is rejected (`resolve_coding_repo_binding`,
`validate_shell_action_hard`); `os_sandbox_command` (macOS `sandbox-exec` /
Linux `bwrap`) makes live source read-only and `magician_data_v3` writable,
default-on after launcher self-test (`MAGICIAN_CODING_OS_SANDBOX=0` disables);
a wrapped launch that then rejects fails closed. Codex/Grok/Claude/Agy need
`require_outer_fence()`; Pi needs `require_pi_sandbox()` (refuses to spawn
unwrapped) unless `coding.pi_allow_unsandboxed: true` (host is its own sandbox).
Without a source checkout the gate still wraps, keeping the real-repo write
deny and credential read denies.

Environment: the spawn and the `--version` probe drop
`operator_terminal_env_scrub_list()` (every runtime `.env` key,
`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `XAI_API_KEY`, Makefile build knobs),
then apply `request.env`: the profile's `api_key_env`, `PI_API_KEY`, citizen
vars and `CARGO_TARGET_DIR` = `coding.build_dir` (absolute) or
`<coding sandbox root>/coding_engine/build/cargo-target`. A relative,
uncreatable or in-source-tree dir fails at `stage: build_dir`; under
`coding.build_dir_min_free_gb` (default 10) free disk it is emptied first when
no cargo holds it (`coding.build_dir_pruned`). Plane-harness turns keep
`env_clear()`.

## Apply, checks and autopilot

`apply_code_proposal` applies the run's own staged proposal (idempotent
`already_applied` when not Pending); shadow re-sync at the **start of every
turn** wipes an unapplied proposal. `run_project_checks` wraps the shadow
runner. Both are granted to senior/principal/frontend/junior-* (system template
**and** `scopes/anonymous/default`); `notify_owner` to `engineering-manager`.
Autopilot: branch-first (never main), coding → apply → checks → fix (the
directive asks for at most 12 coding iterations;
`vibedev_cockpit_autopilot_directive_block`), commit, `notify_owner`. Only an
Autopilot run (`AgenticContext::vibedev_autopilot`, from the `autopilot` tag,
inherited by delegates and kept across pauses) or a non-VibeDev agent holding
`apply_code_proposal` skips the `diff_approval` park; an attended Build parks
even when its engineer holds that tool. Accept-in-scope skips permission HITL
for in-tree file tools, not `diff_approval`. Answering a finished engineer's
review wakes its parked parent (`schedule_child_completed_wake`); a parent
parked on an open review is not a reconciliation error.

## Preview screenshots

`screenshot_preview` (`project_id`, optional `full_page`; granted to the five
engineer agents) captures the live preview headlessly (60s; soft `{ok:false}`
if none) into a `#vibedev` attachment on synthetic session
`vibedev-shots-<project_id>` (newest-12). IMAGE records become RPC `images[]`
via `PiTurnOptions.images`, **not** a shadow path. Cockpit visual self-correct
is opt-in; its directive asks for at most 3 visual passes per change
(`run_service::vibedev_cockpit_visual_self_correct_block`).

## Citizen API

Extension `magician/assets/pi-extensions/magician-citizen.ts` (embedded,
outside the shadow). `CitizenTokenRegistry` mints a `CitizenGrant` per turn and
**revokes it on settle**; the bearer token **is** the scope.
`POST /api/magician/v2/vibedev/citizen/preview_url` →
`dev_server_manager().status(project).local_url` (loopback).
`POST /vibedev/citizen/secret` → scoped broker (`magician_citizen:dev_server_env`)
→ `set_secret_env` → `{ok, env_var, requires_restart}`, never the value.
Project: explicit arg → grant `project_id` → `active_root_task_id` match.
`magician_code_knowledge` is live; `magician_lsp` is unbuilt.
m6 archive.

## HITL and terminalization

`diff_approval` pauses the execution; `reconcile_stale_active_root_execution`
fails an orphan (runtime gone, no terminal event, non-terminal task, **no**
`resumable_pause_exists`) via `ExecutionOutcomeSnapshot::orphaned_execution`. A
terminal `WaitingState` emits `HitlResolved { source: "diff_approval" }`
(`cancelled` vs `superseded`) before clearing pauses. Reject/apply with no live
pause but `body.execution_id` set drains
`full_pause_store().remove_for_execution`, then `ExecutionFailed` (reject) or
guarded `ExecutionStart`→`ExecutionComplete` (apply).

## Coding budgets

Duration is a backstop; budgets resolve **once** into `ResolvedCodingBudgets`.

| Mechanism | Config | Catches |
|---|---|---|
| No-progress detector | `coding.no_progress.*` | wedged run (silence, not duration) |
| Cost ceiling | `MAGICIAN_AGENTIC_MAX_COST_USD` + token meter | runaway spend |
| Loop detector | executor `LoopDetector` | semantic repetition |
| Turn wall clock | `coding.turn_timeout_secs` (24 h, `0` = none) | a bug in the above |
| Whole-task ceiling | `coding.task_budget_secs` (**`0` = none**) | opt-in hard cap |

Defaults: `turn_timeout_secs: 86400`, `task_budget_secs: 0`,
`verification_reserve_secs: 1200`, `no_progress.enabled: true`,
`model_idle_secs: 900`, `tool_idle_secs: 0`, `tool_max_secs: 0`,
`compaction_max_secs: 900`, `summarization_max_secs: 900`,
`retry_grace_secs: 120`. Legacy `timeout_secs` is accepted (conflicts
rejected); `0` must never clamp to `1` (`budgets::clamp_turn_timeout_secs`); the
handler ignores the pack-YAML `PACK_DEFAULT_TIMEOUT_MARKER`.
`coding_execution_max_duration()` (`run_coding_task`, and
`execute_agentically` when `execution_is_coding`) ignores the generic 24-hour
agentic default: **`None`** without a task ceiling, explicit
`MAGICIAN_AGENTIC_MAX_DURATION_SECS` honoured (`0` = unbounded).
`task_active_max` is `Option`; unbounded `remaining` is absent, not zero.

`ProgressWatchdog` (in `collect_events_until_boundary`; identical/empty events
reset nothing, fingerprint ring of 8):

| Phase | Advanced by | Bound |
|---|---|---|
| model | substantive text/thinking delta | `model_idle_secs` |
| tool | real output or tool state change | optional `tool_idle_secs`, `tool_max_secs`; zero disables each |
| compaction | — | `compaction_max_secs` |
| retry wait | — | declared `delayMs` + `retry_grace_secs` |
| summarization retry | — | `summarization_max_secs`, or a longer declared delay |

The tool phase lasts until the **last** in-flight `toolCallId` ends; a
configured max-runtime anchors on the **first**. Quiet tools otherwise have no
cutoff beyond their own deadline, parent cancellation and enclosing allowance;
after the last tool ends, model-idle detection resumes.
`make test-long-running-harness` drives injected monotonic time past 24 hours
to check backstops, stalls and cancellation in seconds. Escape hatches:
`coding.no_progress.enabled: false` (disarms it),
`coding.no_progress.tool_idle_secs`.

`CodingTerminationReason` (first writer wins): `TurnTimeout`, `TaskBudget`,
`NoProgress { phase, elapsed, last_substantive_event }`, `OwnerCancelled`,
`ParentDeadline`, `CostBudget { limit_micro_usd, spent_micro_usd }`,
`ServiceShutdown`. Payloads carry `termination` + `budget_stop` (true for every
budget stop, cost included).

**Run dollar cap** (`cost_budget.rs`): the cockpit's `- Cost budget: $X` line,
read from the task's control region (`parse_vibedev_cost_budget_usd`; ignored
inside the user prompt fence; lowest line wins). Each turn's
`CodingTurnUsage.cost` (when `cost_known`; else `unpriced_turns`) goes to
`<task_dir>/coding_cost_ledger.json`. Every engine: `run_coding_task` refuses
with `stage: cost_budget_preflight` once the cap is reached. Pi also stops
mid-turn with `CostBudget` on the message that reaches the cap:
`ProgressWatchdog` adds each assistant `message_end`'s `usage.cost.total`, even with `no_progress.enabled: false`;
other engines report at turn end, so their cap bites next turn. A run's
parallel turns share one in-process meter. Payload `budget.cost`: `limit_usd`,
`spent_usd`, `remaining_usd`, `unpriced_turns`. Coordinator model calls are not
counted.

`CodingTaskBudgetLedger` (`<task_dir>/coding_budget_ledger.json`, keyed on the
**root** task): active time is the **union** of intervals, excluding
diff-approval time and declared backoff; a mid-turn crash seals at one turn on
reload; it stops work only when a ceiling is set (`verification_reserve_secs`
held back); a corrupt ledger → in-memory + warn.

## plan_only

Discuss / plan run: `plan` tag (`current_is_plan`) **or** `plan_only` arg →
`stage_result=false`, so no byte-diff and no `diff_approval`. Plan text goes to
temp `plan.md` (existing tool-output capture); `coding.completed` carries
`plan_run` and `assistant_text` via `truncate_chars` (~20k), while build runs
keep the 500-char `trim_for_event` preview. Empty or failed write →
`coding.plan_artifact_not_captured`. See
[agentic-loop-termination.md](agentic-loop-termination.md).

## Checkpoint rewind

Every checks-passing apply mints a checkpoint (`mint_checkpoint_on_green`):
`git_sha` on `refs/vibedev/checkpoints/<id>` (throwaway `GIT_INDEX_FILE`),
`repo_path`, native `source_session_id`.
`POST /vibedev/runs/{task_id}/checkpoints/{checkpoint_id}/revert` snapshots the
worktree (`undo_ref`), then `git restore --source=<sha> --worktree -- .` (keeps
later files; never touches branch/index; 400 without a git anchor), and writes
`set_pending_engine_resume` under
`<scope_root>/checkpoints/.pending_pi_resume/<task_id>`. The next turn binds the
native id **only when** `engine` matches (`pi` / `codex_app_server` /
`grok_acp`); Pi → `resume_session_id` → `--session <id>`. Cockpit: hover ⟲ on
git-anchored nodes, behind a confirm.

## Model, vision, `/llm`

Profile order: tool `coding_profile`/`profile` → agent's
`llm_routing.coding_profile` → `coding.default_profile`. Catalogue (`coding.profiles`): GPT-6.1 Sol
(`coding-balanced`, 64k output, the default; `coding-premium`), GPT-6 Astra
(`coding-astra`), Claude Opus 5 (`coding-opus5`), Claude Opus 5.5
(`coding-opus55`), Claude Fable 5.1 (`coding-fable`), GPT-6 Luna
(`coding-luna`), Grok 4.7 (`coding-grok47`), DeepSeek V4.1 Flash
(`coding-deepseek-v4.1-flash`; screenshots and thinking on one model), MiniMax
M3 (`coding-minimax-m3`).

Provider and model go to Pi as `--provider` (magicllm `gemini` becomes
`google`) and `--model`, with `api_key_env` forwarded (Pi reads `XAI_API_KEY`
for `xai`). Billed cost is Pi's stats delta, only as right as Pi's catalogue:
1.3.0 lists every shipped coding model; an unknown id runs on a clone of the provider's default entry (warning
"Using custom model id") at its rates.

`/llm` records only `LLMResponseReceived`: `run_turn` samples stats before the
turn and on every terminal path (error/timeout/cancel: 5s bound) and writes the
**delta** (`CodingTurnUsage`) to `usage_capture`; `emit_llm_call` maps it
(`operation`/`response_kind` = `"coding"`), skipping zero spend and resumed
sessions whose `before` sample failed. One row per run (call count undercounts;
cost/tokens exact). Cockpit `% ctx` reads full `CodingSessionStats`;
`CodingContextUsage.tokens`/`percent` are `Option` (`null` after compaction /
before the first response), `percent` 0–100 unscaled.

## Conformance

`pi::tests::pi_rpc_conformance_when_enabled` (no-op without the env var): warm
`pi --mode rpc`, prompt, intermediate `agent_end`, terminal `agent_settled`,
stats + last text.

```bash
make setup-pi-coding-agent
MAGICIAN_PI_CONFORMANCE=1 make cargo ARGS="test -p magician --lib pi_rpc_conformance -- --nocapture"
```

Not supported: cross-request warm pool, periodic cost events, mid-session
`set_model`, binary files in proposals, `magician_lsp`; one shadow per repo.

Codex app-server puts declared `mcpToolCall`, `dynamicToolCall` and `webSearch`
item start/end (MCP progress by correlated item ID) in the tool phase, so a
quiet governed MCP call is not taken for a silent model (verified with injected
time).
