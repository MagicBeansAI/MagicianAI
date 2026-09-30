# Chat Mode Architecture

## Chat turn engine

`chat.harness_engine` (default `magician`) selects who thinks the **mouth** of
a chat turn. Roster names (`claude_code`, `codex`, `codex_app_server`, `grok`,
`agy`) run a harness session whose only governed hands are the Magician plane.
The seam is `maybe_harness_chat_turn` inside `process_chat_inline_turn` —
streaming and non-streaming HTTP share it. Voice/meeting, App Copilot, Tutor,
public envoy, and disclosure-guarded turns stay on the Magician LLM.
A turn on a roster harness names that engine as the **parent engine** of its
background LLM operations (`chat_turn_parent_engine`, scoped around
`process_chat_inline_turn` and re-scoped across the turn's execution-runtime
hops; `magician` names none), so the turn's text-only operations — memory,
retrieval, summaries, the tools a bridged name reaches — follow the mouth
unless the operator pins one (`llm-routing-overrides.md`).
Everything handed to a harness is provider-bound: `turn_input_text` and
`HarnessSessionRequest.system_prompt` pass `sanitize_text_for_provider` — the
same redaction the Magician LLM path applies — before the harness sees them.
Native-tool strip is per engine (`native_tool_posture` on
`GET /plane/engines`): claude_code and grok empty their built-in registries
(stripped); Codex and agy have no empty-registry flag and run sandboxed.
Every engine but `codex` streams its reply live (`codex exec --json` emits
completed items only, so its reply is sent as one chunk once the turn
settles); a streamed reply is persisted whole from the collected deltas,
and a turn that streamed nothing sends its settled text as one chunk
whatever the engine advertises. Every engine resumes its own native
session between turns from a per-conversation home (a restart or an engine
switch is one cold turn that replays the transcript, tool results included);
chat-runtime, structural and persona tools reach a swapped mouth through the
mouth bridge under the native dispatcher's rules. Inline
adapters are Magician-mouth only. Settings → Engines has an
install-aware chat-mouth picker (`PUT /plane/chat-engine`) that writes the YAML
key and reloads the live snapshot. See [plane.md](plane.md) and
the chat-turn plan.

## Two-tier execution model

Chat exposes exactly two execution paths to the LLM:

1. **Inline tool** — synchronous call. MCP tools, fast reads (`list_tasks`,
   `read_file`), chat-runtime tools, control tools. The tool-loop blocks on
   a JSON result.
2. **Task** — every heavyweight execution is a real V3 task. `Deferred`
   (default `create_task`) returns `task_id` immediately. `Await` (used by
   `dispatch_capability_pack`) blocks on terminal status.

There is no chat-pack path: no `chat-pack-exec-<uuid>` ids, no
exec-id-keyed fan-out, no `ChatMessageContent::PackProgress`. Pack calls
mint a V3 task. The only fan-out registry is `chat_fanout_by_task` with
`register_chat_fanout_for_task` / `unregister_chat_fanout_for_task`. Every
chat-spawned execution renders as a `TaskStatusUpdate` card.

`TaskLifecycle::Persistent` (default) is user-visible on `/tasks` and
survives chat clear/delete (`create_task`, plan-mode, scheduler,
autonomous). `TaskLifecycle::Internal` is chat/runtime-spawned work
(pack-dispatch, default delegate/handover, sub-goals, debug runs), stored
under `internal_tasks/`, listed on `/internal-tasks` and
`/tasks?type=internal`. Auto-cleanup is keyed on `chat_session_id` via
`cleanup_ephemeral_tasks_for_session`; Internal tasks with no session id
are never auto-swept. On-disk `ephemeral_owned_by_chat` /
`internal_debug` deserialize as `Internal`.
`CreateExecutionRequest.debug: true` writes `created_by: "__system__"` so
`task_is_user_visible` hides the run; lifecycle stays Persistent.

Outside-born tasks do not appear in chat by default.
`subscribe_to_task(task_id)` (personal-agent-only) attaches the session to
a live stream — single-slot, backed by
`GET/POST/DELETE /api/magician/v2/chat/sessions/{id}/tailed-task`. Tailing
an ephemeral task queues inbound messages in `pending_messages` until the
task settles; persistent `create_task` work does not auto-tail.
`QueuedMessageReceipt.reason` is `InFlightTurn` or
`TailingTask { task_id }` (`current_tailed_task_id` wins).
`chat_task_subscriptions` keeps create/subscribe/await subscriptions
alive past turn end; `drop_chat_task_subscription` is the shared GC;
inserts require `active_chat_runs.contains_key(session_id)`.

Task execution events fan out into
`<scope>/ui/chat_turn_events/chat-task-<task_id>-<session_id>.jsonl` and
render under the task card via `RequestActivityCard`.
`project_task_outputs_for_chat_session` copies files on every
`StatusChanged` (dedupe `(source_task_id, source_task_output_id)`).
While synthesis is pending, `apply_synthesis_pending_status_guard` holds
visible status at "running"; `ChatChannel` stamps `synthesis_pending` and
delays fan-out teardown. The landed card carries the canonical user
summary, `output_files`, and optional `speech_tts`.

History reads reconcile non-terminal task cards against canonical V3
state (terminal wins immediately; deleted internal tasks render
cancelled). Startup orphan recovery repairs the activity stream and the
durable card. Terminal dedupe includes the execution id and closes
execution-owned HITL. `create_task` accepts `reference_task_ids` (always
persistent; no lifecycle overrides); delegate/handover/pack/`spawn_sub_goal`
accept them for Internal work. Terminal finalization emits
`role=continuation_context`. Procedure-skill names are canonical slugs;
resolution is literal-only.

Plan:
`docs/archive/plans/2026-05-21-chat-native-task-progress.md`.

## Purpose

Chat is a conversational surface over the user's personal agent. Ask-mode
sends converge on `ChatService::process_chat_inline_turn` (sync HTTP, SSE,
and Tutor background continuation). That function is the outer loop: it
loads the session agent's scoped definition, builds the turn catalog,
renders `chat_outer_loop_system` **v0.0.6**, and iterates
`≤ CHAT_INLINE_MAX_TOOL_LOOP` (50) times with `tool_choice: auto`.
`session.agent_id` is fixed for the whole turn. Typed composer prefixes
(`agent:<id>`, `skill:<name>`, `personality:<name>`) are UI chip sugar and
reach the model as plain text, not a routing override.

`GoalSource::ChatInline` is the source stamp on **spawned** agent cycles
(`delegate_to_agent`, `handover_to_agent`, `spawn_sub_goal`). Those cycles
run through `AgentRuntime::trigger_goal_awaitable_with_scope_and_overrides`
in Do mode (`execute_agentic_direct_with_outcome`). They are not how the
user's ask-mode message itself is answered.

Meeting, App Copilot, Tutor, public envoy, and disclosure-guarded turns
stay on the Magician LLM even when `chat.harness_engine` is a roster
harness. Live/Realtime **`delegate_to_chat`** and hands-free turns think with
the calling client's composer engine (`chat_choice` on `session.start`), else
`chat.harness_engine` (Settings → Engines, "default for all clients", which
also switches every open composer via `chat.engine.updated`): Magician or
Claude Code / Codex / Grok / Agy, with Magician plane tools as hands. GPT-Live-1 is the spoken
mouth; the configured chat mouth is the brain. This is not the VibeDev
coding-engine picker.

## Structured response presentation

Every eligible generic non-user chat message carries a validated,
server-derived `ChatMessage.presentation` sidecar when the complete
canonical output fits V1's bounded envelope
(`magician/src/magician_v2/chat/presentation.rs`, schema
`magician.structured_response`). `plain_text` is a canonical projection of
semantic `content`, not of rendered blocks. Storage accepts a supplied
sidecar only when it exactly equals the deterministic projection; otherwise
it replaces it. Rich output is never truncated to fit a card: an over-bound
result omits its sidecar and continues through canonical rendering.
Artifact targets identify a session output or a scoped task output without
exposing filesystem paths. Active HITL escalation and live task-tail cards
remain authoritative specialized components.

## Unified tool-result and turn-context contract

Chat does not replay complete large dispatcher payloads on every model
iteration. After an authorized tool returns, the shared result runtime
applies the post-dispatch safety guard, materializes one immutable
scope-bound raw result, and derives deterministic model, spoken, and
display views. `ChatLlmTranscriptEntry::ToolResultProjected` persists the
versioned projection and original tool-call identity; provider history
receives only the bounded structured model value. Legacy entries hydrate
without a destructive migration. Projection or materialization failure
produces one balanced typed result for the original call ID — never a
JSON character prefix.

Canonical storage does not guess key names or value shapes. Injected
credentials are removed by exact known-value replacement before
materialization; capability metadata alone declares record paths, priority
fields, atomic groups, and narratable fields. An outbound-only guard
recognizes exact credential protocol fields and validated credential
syntax before provider transport, without mutating stored evidence.
Ambiguous domain fields named `token` / `secret` / `cookie` /
`authorization` survive unless the value is an injected or protocol
credential. The runtime receives a typed `ToolOutcome`; unknown domain
status strings keep the dispatch rail's typed fallback; null
`error`/`error_code` do not manufacture failures.

Large display values continue through authenticated
`POST /api/magician/v2/chat/sessions/{id}/results/read`. The opaque
reference is a locator: every page rechecks scope, session owner, agent,
tool/trust policy, authority revision, retention, cursor, and content
hash. Reconstruction v1 uses deterministic preorder `complete_value`,
`container`, and UTF-8 `string_fragment` units.

Automatic memory and reusable-procedure context start concurrently behind
the shared staged coordinator under
`agent_surface_runtime.context_retrieval.chat`. Fast immutable memory,
hybrid memory, and procedures have independent completed/empty/error/
timed-out states. A slow sibling cannot erase completed evidence;
cancellation clears turn-bound values; completion order cannot alter
merge order. The same relevance query feeds hybrid memory and procedures.
Contradictory content for the same source identity and revision at the
same completeness is rejected as `evidence_revision_conflict`.

### Interrupted tool-history recovery

Native provider replay validates persisted tool calls as atomic batches
before selecting a continuation anchor. A process interruption can leave
an assistant tool-call row durable while results are absent. Complete
pairs remain typed and ordered; unmatched calls, duplicate IDs, and
standalone results are omitted from provider input only — the model gets
an interruption marker, not fabricated evidence.

When a result is missing and there is no later trusted boundary,
provider-native state from that assistant turn onward is cleared in
memory so a newer OpenAI Responses `response_id` cannot continue
interrupted server-side state. The first successful final no-tool
response stores a server-owned `tool_protocol_repair_checkpoint`;
tool-call responses cannot become checkpoints. Durable history is not
rewritten. `make test-provider-replay-live-eval` runs one bounded request
per configured family; OpenAI Responses additionally probes
`response_id` checkpoint/reuse.

## Session Model

Ordinary chat and brainstorming threads may retain multiple active
sessions. Get-or-create reuses only an exact
principal/workspace/thread **and agent** match. Web navigation is
ID-addressed (`?session=<session-id>`): an active sibling becomes
writable; an archived sibling stays read-only and keeps the prior
active session as the return target.

History records persist a `history_lane`. `personal` is `#general` plus
user-created sessions/threads; `automated` is product-generated activity
(screen/tab observation, meeting, contextual-writing, thinking-map,
VibeDev, external-contact). The History drawer keeps Sessions/Threads as
its primary switch and applies the same Personal/Automated filter and
offset pagination to either list. A non-empty search matches thread names
and session titles across both lanes into one recency-ordered page, each
hit tagged `personal` or `automated`. Thread-scoped browsing omits the
lane filter.

Legacy history has no trustworthy provenance: migration treats only
`#general` as Personal (current seed titles `Wtf` and
`hi i think you have access...`; the allowlist is migration-only). New
user-created history is Personal; product callers stamp Automated.
`#general`'s original session carries `is_default_session`; the store
restores it to Active and rejects archive/delete/move.

`GET /api/magician/v2/chat/sessions` and `GET /api/magician/v2/ui-threads`
accept `history_lane`, `q`, `limit`, and `offset` (omit them for the
legacy full list). `GET /api/magician/v2/history/search` requires `q`
(≤120 Unicode scalars; offset ≤100,000), merges both lanes, and loads
full records only for the requested page. UI-thread startup migrates the
existing table before applying current indexes. Product callers of
`GET /chat/active` or `POST /chat/new` pass `history_lane=automated`;
omission remains `personal`.

**New Chat** does not auto-archive the prior session on user threads.
Feature/rotation threads (`screens`, `tabs`, `vibedev`) keep one rolling
session per exact agent via `chat::storage::thread_rotates_sessions`.
Archived sessions stay read-only history. Channel is origin, not a
partition; enrollment resolves identity to principal first and
serializes **under** the write guard so two writers cannot land an older
snapshot last.

## Runtime Architecture

Ask-mode orchestration lives in
`magician/src/magician_v2/chat/service.rs` (`ChatService`), persistence in
`chat/storage.rs`, models in `chat/models.rs`, the LLM client in
`chat/llm_service.rs`, chat-only tools in `chat/tools_runtime.rs`. HTTP
handlers are `magician-api/src/chat_api.rs`, mounted from
`magician-bin/src/main.rs`.

`ChatService` persists the user turn, then runs
`process_chat_inline_turn`. Transient chat-owned work is
`TaskLifecycle::Internal` with `chat_session_id` under `internal_tasks/`.
Terminal cards copy task outputs into the session output store so they
remain openable after the internal task is removed. Chat-inline native
requests use `tool_choice: auto`; assistant text is a valid terminal
reply. After prompt render, a "Recent Tasks In This Thread" section
(last 5, `ui_thread_id`, wrapped in `<task_context>`) and a bounded
"Recent Session Files" section are appended. Sync and streaming
completions both resolve through one authoritative `messages[]` batch.

Once a `ScopedCapabilityResolver` is configured, the turn resolves one
immutable registry/schema-index pair in `tokio::task::spawn_blocking`.
Failures **fail closed** (no global-registry fallback). Chat, realtime
voice, and autonomous tasks share one revision-bound surface-plan cache
and working-set store; surface keys isolate sessions, calls, owner
frames, and feature modes. An authority-revision change clears the
loaded set. Chat `tool_search(select:...)` is a solo provider call and
commits the selection for the **next** iteration; realtime voice
sends a correlated catalog update and withholds the tool result until
acknowledgement.

Chat and realtime voice read the snapshot's **surface tool index**
(`ScopedCapabilitySnapshot::surface_tool_index`,
`flat_loop::build_surface_tool_index`), not the executor's leaf index: a
multi-primitive pack (`browser`, `duckdb`, every CLI skill with typed actions)
is one pack-named tool there, carrying the pack-level parameters
(`command`, …) and a description that lists its actions so keyword search
still finds it. These surfaces dispatch whole packs — a pack call mints a
task-backed sub-run scoped to that pack, whose own loop drives the leaves —
and have no leaf dispatcher, so a leaf name like `browser__open` must never be
advertised (it would scope a sub-run to a pack that does not exist). The pack
tool's text says what it is — "call this ONCE with the complete objective; a
dedicated run performs every step itself and returns the outcome; never
one action or command per call" — and its intent parameter (`command`,
`goal`, …) is re-described as the complete objective: the pack's own
description names its actions as CLI commands, and a model that splits them
into one call per action gets a fresh sub-run each time with no page to act on.
Every pack sub-run attaches to
the chat thread's shared browser window (`magician-chat-<thread>`, the same
one delegations and handovers use), so a call later in the same task sees
the page the previous one left. The window ends with the task that used it:
dispatch records the thread session among the run's sessions beyond its own
(`browser_run.additional_session_ids`), and terminal cleanup closes it (a chat
task never opens `magician-<execution>`, so closing only that would leave the
owner's Chrome attached and "being debugged"). On the owner's Chrome (CDP) the daemon exits without
`Browser.close` (the websocket closing detaches the debugger) and Magicutor's
`DELETE /cdp/threads/<session>` closes the window the automation opened,
leaving a tab the owner had open alone. The window stays only when the task
asked (`keep_browser_window_open`, `keep_browser_cdp_connection_alive`) or
paused; the next task on the thread opens a fresh one. A `tool_search(select:browser__open,…)` on this index
loads the `browser` entry — a `<pack>__<action>` name the index does not
carry resolves to its pack (`ToolIndex::pack_for_selected_name`), because
a model that learned the leaf names from a run keeps asking for them —
and the result says whether anything loaded: an empty select answers
"nothing was loaded", and a keyword listing carries `how_to_load`
(a listing is not a load). A pack
that declares a `chat_inline_adapter` keeps its leaves (the adapter
dispatches them inline). Should a leaf name still reach
`dispatch_capability_pack`, it is scoped to the pack the leaf belongs to and
its goal carries the user's request plus the chosen action
(`chat_pack_dispatch_target`, `chat_pack_leaf_goal_text`).

`FilePublicContactProfileStore` (`chat/public_contact_profile.rs`)
persists per-sender identity/research.
`GET /api/magician/v2/chat/public-contacts` and
`GET /api/magician/v2/chat/public-chat/status` expose it. Live queues and
daily budgets persist under `system/public_chat/runtime_state.json`.

`ChatMessageContent`: `Text` (optional `plan_reply`), `ToolCallExecuted`,
`RichToolResult`, `Attachment`, `TaskStatusUpdate` (`synthesis_pending` /
`speech_tts`), `Escalation`, `EscalationResolved`. No `PackProgress`.

## Tool Model

Task controls live on the execution-native agentic surface:
`list_tasks`; compiled `get_task_details`
(`execution/embedded_pack_defs/get_task_details.yaml`) plus the richer
chat-runtime `get_task_details_for_chat`; `create_task` (always
persistent; optional `reference_task_ids` and `TaskSchedule` JSON;
`dispatch_create_task` ignores LLM `agent_id` and uses the thread's
personal agent); `run_task` / `stop_task` / `update_task` /
`delete_task`; `preview_monitor` / `create_monitor` / `update_monitor`
(`create_monitor` requires `preview_fingerprint` — see
[recurring-monitors.md](recurring-monitors.md)); `spawn_sub_goal`,
`delegate_to_agent`, `handover_to_agent`, `find_agents_for_capability`;
`create_dashboard` / `unpublish_dashboard` under
`execution/embedded_pack_defs/` (not `capability_templates/`).

**Adaptive-profile escalation.** When the operation's effective profile is
an adaptive composite, `process_chat_inline_turn` injects
`request_thinking_mode`. The runtime intercepts the call before
persistence, swaps `fast_profile` → `thinking_profile`, and continues.
The tool is removed after the first call (one escalation per turn). See
[chat-profile-routing.md](chat-profile-routing.md).

**Capability packs.** `dispatch_capability_pack` creates a real V3
`Internal` task with `sync_mode: Await`, `created_by: "chat_pack_dispatch"`,
and `chat_session_id`. It subscribes the chat to the task **before**
`activate_execution` so the first `running` card is not missed. Pack
`ImplementationType` is `Composite` / `Compiled` / `Primitive` / `Command`
(no JavaScript). The sole chat-inline adapter is
`ChatInlineAdapter::TutorScreenDraw` — a five-second local host gateway or
immediate realtime-client event, never a V3 task.

**Chat-runtime tools** (`ChatRuntimeTool` in `tools_runtime.rs`) are
injected into the chat surface only: session/thread archive-delete-switch,
`get_current_chat_context`, `record_chat_teaching_feedback`,
`get_task_details_for_chat`, `subscribe_to_task_for_chat`,
`describe_agents_for_chat`, `list_tools_for_chat`, `read_result`, plus
Tutor/App Copilot runtime tools (`start_tutor_run`, …) when the lane
keeps them. Untrusted agents do not receive chat-runtime session/thread
or structural spawn/delegate/handover controls.

**Dispatch chokepoint.** Every chat-spawned tool call goes through
`dispatch_chat_tool_call`. Before any branch it revalidates the turn's
authorization revision, the exact advertised name ceiling
(`allowed_tool_names`), trust policy, `denied_tool_params`, and structural
approval. A policy change while the model was deciding returns
`ChatPolicySnapshotChanged` and executes nothing. Compiled calls from
`reviewed` / `untrusted` agents bypass the chat fast path and enter the
task-backed executor; `builtin` / `local` chat retains the compiled fast
path. Memory-bridge parameters overwrite public `agent_id` with the bound
chat agent.

There is no chat-local `ChatToolRegistry`, YAML chat-tool path, or
`tool_call_proposal` continuation. Domain work is an agent capability, not
a chat-specific tool.

## Composer UX and reference picker

The desktop composer (`FloatingComposer.svelte`) is keyboard-first. Enter
sends; Shift+Enter inserts a newline; Cmd/Ctrl+Enter also sends. The Stop
button materializes only while a turn is in flight and dispatches
`DELETE /api/magician/v2/chat/sessions/{id}/run`. Queue-while-busy still
works (Enter queues); `QueueInspector` above the composer is the canonical
queue indicator.

The `@` picker is text insertion, not a control path: `@agent` →
`agent:<id>` (session agent plus delegates); `@skill`/`@tool` →
`skill:<name>` (delegate-owned as `skill:<name> via agent:<id>`);
`@personality` → `personality:<name>`. Source is
`GET /api/magician/v2/chat/sessions/{id}/reference-catalog`. A missing
session or catalog failure **fails closed** — no global-directory
fallback. Selection does not call `activate_skill` or any control API.

## Transport bus and per-chat-turn events

Every progress / chat / agent event flows over
`RuntimeTransportBroadcaster`. Sinks call `subscribe()` synchronously
**before** `tokio::spawn`.

- **`ChatStoreSink`** — projects `ChatMessageReceived` into the store; on
  `ProgressEvent` calls `ChatChannel::deliver` for `TaskStatusUpdate`
  cards (never `PackProgress`). Sole writer to `chat_store`.
- **`ChatTurnEventSink`** — extracts `(chat_turn_id, principal, workspace)`,
  appends `<scope>/ui/chat_turn_events/<chat_turn_id>.jsonl`, and
  broadcasts for live SSE. Events lacking `chat_turn_id` are recovered via
  `ChatFanoutResolver::resolve_turns` against `chat_fanout_by_task`.
- Persistence sink writes per-scope `events.jsonl`.
- `EscalationListener` + `PlanningListener` emit chat messages onto the
  bus (they do not write the store directly).

`chat_fanout_by_task` copies delegate events under the chat agent's
identity with `chat_delivery_kind: "inline_delegate"`. `tool.call.*`
`call_id` is rewritten to `delegate-{execution_id}/{orig_call_id}`.

Activity endpoints share that projection:
`GET .../turns/{cid}/events` (last 1 MiB /
`CHAT_TURN_EVENTS_TAIL_MAX_BYTES`, at most `limit` newest rows, default
50, `truncated` flag) and `.../events/stream` (disk replay then live,
deduped by `event_id`). The generic
`/api/magician/v3/events?chat_turn_id=…` filter is gone.

`chatTurnEventsStore` reconnects with 1s→30s backoff; a 30s REST watchdog
reconciles subscriptions idle 60s. `liveTurnId` keeps the spawning turn
SSE-subscribed for the delegate lifetime. Inspect opens
`TaskPanelDrawer` via
`GET /api/magician/v3/tasks/{id}/execution-panel?execution_id=…` and
requires both ids; turns with only a chat-turn `execution_id` stay on
`Show all`. Chat-inline LLM calls also emit
`RuntimeTransportEvent::LLMResponseReceived` with
`capability="chat.inline"` — see
[duckdb-analytics.md](duckdb-analytics.md#emit-site-coverage).

## HITL in the chat surface

HITL that fires during an in-flight turn flips the typing bubble to a
"Waiting on you — click to respond" pill
(`pendingHitlByChatTurnId` in `ChatPanel.svelte`). Click opens the same
modal as AttentionBar / `/attention`; the response goes through
`POST /api/magician/v2/hitl/{cid}/respond`.

`EscalationListener` **is** started at boot from
`magician-bin/src/main.rs` and projects canonical HITL events into the
owning active chat session as `Escalation` / `EscalationResolved` cards
(one per `pause_state_id`). `ChatChannel::deliver` hard-skips canonical
`hitl.*` events so chat-spawned executions do not also render a duplicate
Text row. Service-backed tool/sandbox escalations carry `request_id` and
respond through `/api/magician/v2/user-requests/{request_id}/respond`;
native execution-owned pauses use
`/api/magician/v2/executions/{execution_id}/execution/agentic-resume`.

Full contract: [HITL / Attention](hitl-attention.md).

## Chat-run Cancellation and Pending-Message Queue

`ChatService::active_chat_runs: Arc<DashMap<String, ActiveChatRun>>` holds
one `{ token: CancellationToken, owner_id }` per in-flight session turn.
Registration is atomic (`try_register_active_chat_run`). An RAII
`ActiveChatRunGuard` clears the slot on every return path.

`DELETE /api/magician/v2/chat/sessions/{id}/run` →
`ChatService::cancel_chat_run` removes the slot, signals that turn's
token, terminally cancels the scoped tutor run, and best-effort preempts
any pending delegated UI action. When the runaway turn returns, the
wrapper substitutes `ChatResponse { cancelled: true, assistant_message:
None, .. }`. `run_task` / `create_task` do **not** cascade — tasks
outlive chat turns.

`process_chat_inline_turn` races the outer LLM call against
`cancel_token.cancelled()` (dropping the future sends HTTP/2
`RST_STREAM`), checks the token at each tool-loop iteration, and breaks
after persisting an in-flight tool result so remaining calls in that
response are skipped. `process_message_inner` and
`process_message_streaming_with_mode` check cancellation **before**
`persist_user_turn`. Delegate/handover spawn a watchdog (5s `get_task`
poll, 3-hour deadline) that calls
`ArtifactV2Service::cancel_execution_by_id`. Pack dispatch forwards the
token into `PrimitiveExecCtx::cancellation_token`. SSE disconnect wraps
the stream in `SseDisconnectCancelGuard`; `Drop` calls `cancel_chat_run`
unless the watched turn already wrote its response.

**Tutor.** `POST /api/magician/v2/chat/sessions/{id}/tutor/cancel` is
idempotent within the resolved scope and remains usable after the
persisted chat session is missing. Deleting a session invokes the same
tutor cleanup first. Overlay teardown follows the terminal Tutor
lifecycle (`tutor.run.failed` / `tutor.run.completed` publish `idle`).
See [personal-tutor.md](personal-tutor.md).

When a new message arrives while the session already has an active turn,
the atomic claim returns `None` and the dispatch wrapper enqueues into
`pending_messages: DashMap<String, VecDeque<QueuedMessage>>`. Bounded at
`MAX_QUEUED_PER_SESSION = 5` with drop-oldest. After every successful
turn, handlers `tokio::spawn(svc.drain_pending_queue(sid))` when
`pending_queue_depth > 0`. Queue is in-memory only — a supervisor restart
drops it.

REST: `GET/DELETE .../queue`, `DELETE .../queue/{message_id}`. UI:
`QueueInspector.svelte`. Bot SDK: `/stop`, `/queue`, `/clearqueue`.
Decisions:
`docs/archive/plans/2026-05-16-chat-message-queue-and-control-commands.md`.
Streaming details: [chat-sse-streaming.md](chat-sse-streaming.md).

## Outer-loop cleanup

`process_chat_inline_turn` records LLM and transcript-persistence errors
into `deferred_error` and `break`s instead of propagating via `?`. The
cleanup tail (`unsubscribe_turn_subscriptions`, plus
`unregister_chat_fanout_for_task` for any registered task) always runs
before the deferred error propagates.

When `deferred_error` is set, the tail also persists a placeholder
`ChatLlmTranscriptEntry::AssistantTurn` whose text starts with
`[chat-inline] turn failed:` and `provider_state: None`, so the OpenAI
Responses anchor walker does not pin at the prior successful
`response_id`. Persist is best-effort.

On every turn, after `get_llm_history`, `first_chain_poisoning_index`
scans for consecutive user-side entries or a persisted failure-marker
`AssistantTurn`. `heal_poisoned_chain` splices synthetic assistant
markers between adjacent user pairs and strips `provider_state` at-or-after
the poison index. Disk is never rewritten; the heal is in-memory per turn.

## Delegation

Default `GoalSource::ChatInline` delegation creates an `Internal` task
(`created_by: "chat_delegate"`, `sync_mode: Deferred`).
`track_as_task: true` creates a `Persistent` task
(`created_by: "chat_delegate_tracked"`). Cycle creation, scope binding,
and progress fan-out stay identical. A multi-target batch is prepared
behind one launch gate and released only after every child is admitted.

`delegate_to_agent` / `handover_to_agent` accept optional
`personality_mode`. Resolution is
`ChatService::resolve_personality_directive`: omitted / `default` /
`none` / `off` / `inherit` → no override; `current` → caller's
`personality_profile`; any installed personality-mode skill name → that
preset for one cycle. The directive rides
`GoalTaskOptions.personality_directive` and is prepended at the LLM
dispatch site; it does not land on `task.title` / `task.description`. A
non-noop mode stamps the spawning task with `voice:<mode>`.

`find_agents_for_capability` returns reachable agents that own a given
skill/pack, or whose id/name/alias matches (`web_research` finds
`web-researcher`). Optional `required_capability` on a delegation target is
cross-checked; mismatch returns retryable `CapabilityNotOwned`. Ordered
rounds (delegate A, then B with A's artifact ids) are the supported
multi-capability pattern. Chat-inline spawned cycles skip Plan mode, so
`GET /tasks/{id}/plan` returns 404 for them — expected.

## Planning replies in chat

Only unresolved user attention is composer-adjacent: `eliciting`
questions and `draft` plans awaiting review. Selecting **Reply** creates
a plan-reply composer intent with `task_id` / `question_id`.
`ChatService::resolve_plan_reply_target` re-reads the scoped task plan,
verifies the task belongs to the chat session's UI thread, and requires
the question to remain pending. The persisted user text carries a
`plan_reply` envelope. A Plan-mode send without a selected reply target
means "create a new plan"; ask-mode requests containing plan target
metadata are rejected.

## Media and outputs

Attachments upload to the session-local outputs store before send.
Attachment-aware (including attachment-only) sends persist one canonical
`UserTurn` plus display messages.
`GET /api/magician/v2/chat/sessions/{id}/outputs/{path}` serves HTML / PDF
/ SVG / text / audio / video inline via
`task_api_v3::infer_safe_inline_media_type`. `POST .../outputs/open-folder`
and `.../open-file` honor scoped roots or exact paths already referenced
by that session; `open-file` refuses unsafe extensions.

Ask-mode media uses `image-generation`, `video-generation-via-veo`,
`gif-search-via-klipy`, `meme-generation-via-imgflip`. Image-MIME pack
artifacts register `prompt_image: true` for multi-turn replay.
Prompt-visible multimodal input is `image/*` only. Anthropic replays
native `tool_use`, image-bearing `tool_result`, and signed thinking.
OpenAI Responses continues via `previous_response_id`, and so does xAI
Grok (`provider: xai`), which speaks only that protocol — its chat profiles
are a Responses-family replay (`is_openai_responses_family`), image input
gated on `XaiProvider::model_supports_vision`; clearing messages
also clears `llm_history`. MiniMax is text-only for attachments.
`GET /chat/profiles` returns only rich-chat-safe profiles and marks
image-input support.

Retention follows the task's `lifecycle` (`GoalTaskOptions.lifecycle`):
`internal` tasks are swept with their chat session, `persistent` ones stay
(`track_as_task` promotes an inline delegate). `ChatChannel::deliver` copies
outputs into the session store first. `GoalSource::ChatInline` does not imply
user-visible retention — the caller passes `GoalTaskOptions`.

## Server-Published Invoke-Grammar Catalog

`GET /api/magician/v2/chat/invoke-grammar` (stateless, read-only) serves
the invoke grammar of tutor, app_copilot, brainstorm, and vibedev:
`version` (schema only; currently `1`), `etag` (sha256 of the lane set),
`leading_invoke_required` (invokes must open the turn), and
`lanes.<lane>` (`markers`, `spoken_phrases`, `quick_flags`, vibedev
`spoken_shape`). Generated by
`chat::invoke_catalog::invoke_grammar_catalog()` from the parser
constants (`TUTOR_MARKER_INVOKE_WORDS`,
`APP_COPILOT_MARKER_INVOKE_WORDS`, `VIBEDEV_MARKER_INVOKE_WORDS`,
`TUTOR_QUICK_FLAG`, `VIBEDEV_DISCUSS_FLAG`). Agreement:
`magician/tests/invoke_grammar_agreement.rs`. Additive-only. Brainstorm
publishes no spoken phrases (`hey brainstorm` is client-side). Ordinary
`@tutor` / `@copilot` / `@brainstorm` text is not authorization.

## Conversational lane seam

Product lanes register in `magician_v2/chat/lane_seam.rs` instead of
growing new match arms in `chat/service.rs`. `ChatLane` declares
`feature_mode`, `admission_surface`, `matches_session`
(`LaneSessionProbe`), `hot_chat_tools` (`LaneHotToolsProbe`), plus
fail-closed defaults for `keeps_tutor_runtime_tools`,
`injects_personal_tutor_instructions`, `promotes_tutor_screen_draw`,
`narrow_delegation_targets`, `same_lane_invoke`, and
`participates_in_narration_approval_sweep`.

`registered_lanes()` order: Brainstorm, VibeDev, Tutor, App Copilot.
`authenticated_product_lane` decides session-authenticated admission.
`hot_chat_tools_for_feature_mode` supplies the surface-hot set (voice
adds `list_tasks` beside baseline recall). Identity constants
(`BRAINSTORM_FACILITATION_AGENT_ID`, `THINKING_MAP_THREAD_ID`,
`THINKING_MAP_PRODUCT_SOURCE_KEY`) live in the seam.

Tutor/App Copilot admit on a leading `@tutor`/`@copilot` invoke conjoined
with a recognized product-source label. VibeDev admission stays inline in
the service (load-bearing place in the chain, ahead of realtime-voice)
but the decision content is `vibedev::rail::{rail_admits_turn,
rail_turn_lane, vibedev_rail_reply_key}`. `@vibedev` is the only point
in a chat turn that short-circuits into creating a task instead of
replying; see [vibedev-rail.md](vibedev-rail.md). Active-run
continuation, tutor-quick allowlists, screen-draw grant widening, and
voice takeover remain in the service because they read state the seam
does not own. Characterization oracles:
`magician/tests/lane_admission_oracles.rs`.

## Context Retrieval Latency

Memory and reusable-procedure retrieval run concurrently during prompt
assembly from one semantic relevance query. Ownership, scope, and
session audit identity stay out of the query. Identical in-flight
embeddings coalesce; a successful vector is retained 25 ms (key:
endpoint, model, dimensions, embedding context, batch-token contract,
exact query). Failures are never retained.

Every normal turn emits `[CHAT-RETRIEVAL]` with `memory_ms`,
`memory_index_query_ms`, `memory_user_render_ms`,
`memory_agent_render_ms`, `procedures_ms`, `concurrent_wall_ms`, and
`overlap_saved_ms`. User and agent memory share one hybrid score query
and then render on separate cancellation-owned Tokio tasks; assembly
order is user-then-agent. `concurrent_wall_ms` is the critical-path
delay. Prompt usage persistence is a background worker after selection.

```bash
make benchmark-chat-context-retrieval
```

Override via `CHAT_CONTEXT_RETRIEVAL_EVAL_ARGS`; reports under
`coverage/evals/chat-context-retrieval/`. Provider-free regressions:
`make test-chat-context-retrieval-eval`. Resident-model gate:
`make test-chat-context-retrieval-live-eval` (also in
`make test-live-evals`).

## User surfaces

Routes: `/chat`, `/t/[name]`, HUD `/hud`, plus the shell bubble/overlay.
Web chat groups consecutive `task_status_update` messages into one panel
per task wave (display-only); channel clients keep the raw stream. MUIJ
`agent.ui.*` deltas stay off the transcript. Per-message delete and bulk
clear (`DELETE .../messages[/{message_id}]`) preserve the session; bulk
clear walks every segment. Web chat and bots share this API; channel
delivery is in [Consumer Channels](../../consumer_channels.md).

Mounted under `/api/magician/v2` from `magician-bin/src/main.rs`:
`GET /chat/active|profiles|sessions[/{id}]`, `POST /chat/new`,
`GET .../reference-catalog`, `POST .../messages` and `.../messages/stream`,
`DELETE .../run`, queue + tailed-task CRUD, turn `events` + `events/stream`,
`POST .../results/read`, session `outputs` + `open-folder`/`open-file`,
`POST .../tutor/cancel`, `GET /chat/invoke-grammar`, public-contact
status/list, `POST /chat/enroll` and `/enroll/approve`.

## Latency posture (chat-only)

Chat-perceived latency is dominated by visible-token streaming. The chat
outer-loop system prompt (`chat_outer_loop_system` v0.0.6) carries a
**Response shape** section that biases toward ~3–5 sentence replies
unless the user signals depth, matched by `max_output_tokens` on the two
OpenAI chat profiles (`chat-gptterra-responses-vision-toolsauto-fast` at
4,096; `chat-gptterra-responses-vision-toolsauto-thinking` at 8,192).
Agentic / memory / mining / planning profiles stay on their existing
budgets.

Loom (`brainstorm-facilitator`) is not part of that global chat route.
Its turns resolve `brainstorm_facilitation`, which ships on
`chat-gpt6luna-responses-vision-toolsauto-fast`. The chat runtime ignores
generic profile overrides for Loom and permits only OpenAI GPT-5.6
Terra/Luna models for that operation; a missing or invalid mapping is
pinned to the exact mini fallback and never falls through to GPT-5.6.

## Current boundaries

- Chat storage is separate from task threads and planning state.
- Realtime voice authority is never inferred from `voice_origin` (that
  flag is presentation/TTS only). The authenticated server voice bridge
  mints `authenticated_realtime_voice` and requires the stored session
  owner to match the requested agent.
- Public Envoy chat clears the native catalogue; Envoy's untrusted
  definition separately denies `search_memory`.
- Chat omits autonomous-only `yield` / `need_user_input`.
- DuckDB `spawn_blocking` cannot be cancelled by future drop.

## Accept-in-scope is a per-scope preference, not per-session state

A turn carries its `ChatMessageMode` (`ask` / `accept_in_scope` / `plan`), and
`ChatService` mirrors the accept bit into `accept_in_scope_sessions` so a deep
pack sub-execution inside the same turn can see it. That map is in-memory
carry-through for one turn — it is not the preference, and it is not durable.
Every send overwrites it, and `clear_chat_session_state` must remove it
alongside the other per-session maps, or a stale permission bit outlives the
session.

The durable answer lives on `UiPreferences::composer_permission_mode`, stored
per principal + workspace, defaulting to `ask` and normalising to `ask` on
anything unrecognised. See
[composer permission mode](../unified-ui/composer-permission-mode.md) for what
the posture actually waives — narrower than the name suggests — and
[the endpoint contract](../magician-api/ui-preferences-endpoint.md).

Related: [plane.md](plane.md), [chat-profile-routing.md](chat-profile-routing.md),
[chat-sse-streaming.md](chat-sse-streaming.md), [hitl-attention.md](hitl-attention.md),
[personal-tutor.md](personal-tutor.md), [vibedev-rail.md](vibedev-rail.md),
[envoy-agent.md](envoy-agent.md).
