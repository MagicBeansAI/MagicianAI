# Chat Mode Architecture

## Chat turn engine

`chat.harness_engine` (default `magician`) picks the **mouth** of a chat turn.
Roster names (`claude_code`, `codex`, `codex_app_server`, `grok`, `agy`) run a
harness session whose only governed hands are the Magician plane. Seam:
`maybe_harness_chat_turn` inside `process_chat_inline_turn` (streaming and
non-streaming HTTP). Voice/meeting, App Copilot, Tutor, public envoy and
disclosure-guarded turns stay on the Magician LLM.

- **Parent engine.** A roster turn names its engine as parent of its background
  LLM operations (`chat_turn_parent_engine`, scoped around
  `process_chat_inline_turn`, re-scoped across execution-runtime hops;
  `magician` names none), so memory, retrieval, summaries and bridged tools
  follow the mouth unless pinned (`llm-routing-overrides.md`).
- **Provider-bound input.** `turn_input_text` and
  `HarnessSessionRequest.system_prompt` pass `sanitize_text_for_provider` (the
  Magician LLM path's redaction) before the harness sees them.
- **Native tools.** Per engine (`native_tool_posture` on `GET /plane/engines`):
  claude_code and grok empty their built-in registries (stripped); Codex and agy
  have no empty-registry flag and run sandboxed.
- **Streaming.** All but `codex` stream live (`codex exec --json` emits
  completed items: one chunk at settle). Replies persist whole from the deltas; a
  turn that streamed nothing sends its settled text as one chunk whatever the
  engine advertises.
- **Sessions.** Each engine resumes its native session from a
  per-conversation home; a restart or switch costs one cold turn replaying the
  transcript, tool results included. Chat-runtime, structural and persona tools
  reach it via the mouth bridge under the native dispatcher's rules; inline
  adapters are Magician-mouth only.

The install-aware Settings → Engines picker (`PUT /plane/chat-engine`) writes the
YAML key and reloads the live snapshot. See [plane.md](plane.md) and
the chat-turn plan.

## Two-tier execution model

1. **Inline tool**: synchronous (MCP tools, `list_tasks`, `read_file`,
   chat-runtime and control tools); the loop blocks on a JSON result.
2. **Task**: every heavyweight execution is a V3 task. `Deferred` (default
   `create_task`) returns `task_id` at once; `Await` (`dispatch_capability_pack`)
   blocks on terminal status.

No chat-pack path exists (no `chat-pack-exec-<uuid>`, no exec-id fan-out, no
`ChatMessageContent::PackProgress`). The only fan-out registry is
`chat_fanout_by_task` (`register_chat_fanout_for_task` /
`unregister_chat_fanout_for_task`); every chat-spawned execution renders as a
`TaskStatusUpdate` card.

**Lifecycle.** `TaskLifecycle::Persistent` (default; `create_task`, plan-mode,
scheduler, autonomous) shows on `/tasks` and survives chat clear/delete.
`TaskLifecycle::Internal` (pack-dispatch, default delegate/handover, sub-goals,
debug runs) lives under `internal_tasks/`, listed on `/internal-tasks` and
`/tasks?type=internal`, swept by `cleanup_ephemeral_tasks_for_session` keyed on
`chat_session_id` (no session id: never swept). On-disk
`ephemeral_owned_by_chat` / `internal_debug` read as `Internal`.
`CreateExecutionRequest.debug: true` writes `created_by: "__system__"` so
`task_is_user_visible` hides it (still Persistent).

**Tailing.** Outside-born tasks are not shown by default.
`subscribe_to_task(task_id)` (personal agent only) attaches one live stream
(`GET/POST/DELETE /api/magician/v2/chat/sessions/{id}/tailed-task`). Tailing an
ephemeral task queues inbound messages in `pending_messages` until it settles;
persistent `create_task` work does not auto-tail. `QueuedMessageReceipt.reason`:
`InFlightTurn` or `TailingTask { task_id }` (`current_tailed_task_id` wins).
`chat_task_subscriptions` outlive the turn (GC: `drop_chat_task_subscription`;
inserts require `active_chat_runs.contains_key(session_id)`).

**Cards.** Task events fan out to
`<scope>/ui/chat_turn_events/chat-task-<task_id>-<session_id>.jsonl`, rendered by
`RequestActivityCard`. `project_task_outputs_for_chat_session` copies outputs into the session store
(openable after the internal task is removed) on each `StatusChanged` (dedupe `(source_task_id, source_task_output_id)`).
`apply_synthesis_pending_status_guard` holds status at "running" while
`ChatChannel` stamps `synthesis_pending` and delays teardown; the landed card
carries the user summary, `output_files` and optional `speech_tts`. History
reads reconcile non-terminal cards against V3 (terminal wins; deleted internal
tasks render cancelled); startup orphan recovery repairs the activity stream and
the durable card; terminal dedupe includes the
execution id and closes execution-owned HITL.
`create_task` takes `reference_task_ids` (always persistent, no lifecycle
overrides);
delegate/handover/pack/`spawn_sub_goal` take them for Internal work. Terminal
finalization emits `role=continuation_context`. Procedure-skill names are
canonical slugs, resolved literally. Plan:
`docs/archive/plans/2026-05-21-chat-native-task-progress.md`.

## Chat pack calls: pre-decided vs model-driven

A chat pack call always runs as an Internal V3 task
(`dispatch_capability_pack` → `ArtifactV2Service::run_chat_pack_inline`).
`chat::pack_direct::predecided_chat_pack_call` returns a `ChatPackDispatch`:

- **`Predecided(PreDecidedPackCall { leaf, args, tool_call_id })`** when the call
  names exactly one typed action leaf (`<pack>__<action>`, or a pack with one
  native action) with a flat route (`classify_flat_route` ≠ `Unknown`) and
  arguments valid against the leaf schema (`additionalProperties: false`). Call
  id `predecided:<chat call id>`. No model is asked; the leaf runs through every
  gate and the terminal folds into the same APPLY (one iteration). Run side:
  [Agentic execution: pre-decided decisions](execution/AGENTIC_EXECUTION_DESIGN.md#pre-decided-decisions-chat-pack-direct-dispatch).
- **`Model { fallback }`** otherwise, logged as
  `[CHAT-PACK] pre-decided dispatch fallback` (`disabled` at debug):

| Code | Meaning |
|---|---|
| `disabled` | Switch off |
| `no_registry` | Turn has no registry |
| `not_single_action` | Pack without exactly one typed action |
| `no_flat_route` | Unknown pack, leaf or route |
| `schema_invalid` | Failing field paths named; values never echoed |
| `unusable_schema` | Leaf schema does not compile |
| `reserved_parameter` | Leaf declares, or args carry, a key lowering strips/lifts/acts on (`task_state_action`, plan-step or vision signal, `parameters` wrapper, credential reference) |

A `thinking` annotation is not a fallback: it is set aside for validation, the
call keeps its original arguments, and run-loop lowering strips it.

Pre-decided results: **success** puts the whole raw output in `result` (boundary
tags inert; parseable `{`/`[` output as JSON, else a string), `summary`
`` `<tool>` completed. ``, no `deliverable`; the chat projection still bounds
it with a `result_ref` handle. **Failure** puts the neutralized error in `error`
and `summary`. **Approval rules** still pause (`paused: true`,
`terminal_decision: waiting_for_confirmation`); resume re-emits the same call.
Model-driven runs keep the capped `deliverable` and first-line `summary`.

The model never sees the mode; it is recorded in `execution.runtime_handoff`
(`pack_dispatch_mode`: `predecided` | `model`; `direct_fallback`: code or
`null`) and `ExecutionState.pack_dispatch_mode` (stamped at handoff, kept by the
terminal projection).

Kill switch `chat.chat_pack_direct_dispatch` (default `true`; the chat-engine
writer preserves it) feeds `ChatHarnessSnapshot.pack_direct_dispatch` at boot
and on `POST /settings/magician-config/reload`. `false` makes all calls model-driven (`direct_fallback: disabled`). Design:
`docs/archive/plans/2026-10-08-chat-pack-direct-dispatch-design.md`.

## Purpose

Chat is a conversational surface over the user's personal agent. Ask-mode sends
(sync HTTP, SSE, Tutor continuation) converge on
`ChatService::process_chat_inline_turn`, which renders
`chat_outer_loop_system` **v0.0.7** over the agent's scoped catalog and iterates with `tool_choice: auto` up to
the **backstop** `chat.harness_turn_max_iterations` (default 200; 0 = default),
read once per turn. A native turn has no wall clock. The prompt is need-based
(stop when every part is answered, never repeat an identical call); its
"Finishing a request" section requires answering or naming every open ask,
resolving references from records in reach before re-asking, and never calling
information missing that a tool result holds.

**Backstop hand-off.** A turn reaching the backstop while working makes one
tool-less call for the same profile
(`ChatLlmClient::generate_final_response_without_tool_use`):

- Native tool-block replay advertises tools with
  `tool_choice: {"type": "none"}` (Anthropic rejects `tool_use`/`tool_result`
  history without `tools`; every magicllm adapter maps it to its provider's
  "none"); flattened transcripts send none.
- The reply (done, remaining, next steps) is stored as the turn's assistant
  entry with provider state. On failure, 90 s timeout, or no text / only tool
  calls it is a deterministic progress note from the turn's tool record (step
  count, calls and successes, last few distinct tools, "Reply 'continue' and
  I'll pick up from here."),
  stored without provider state; turns holding governed app data or local-only
  app memory always use the note.
- Events: `llm.requested` / `llm.succeeded` / `llm.failed` with
  `purpose: backstop_handoff` under the turn's trace root (usage counted);
  `loop_exhausted`; `chat.turn.stopped` with `reason: turn_iteration_backstop`
  and `handoff: model | progress_note`.
- Stop during the hand-off cancels the turn. Stall, spin, Stop and errors never hand off. On the
  last iteration a spin stop wins, an answer is not handed off, and a
  `continue` counts as reaching the backstop.

**Liveness** (`chat::turn_liveness`) ends a turn early only when:

- **Stalled**: no progress for `chat.harness_turn_stall_seconds` (default 300)
  with no model or tool call in flight. Cancels like Stop, persists a notice,
  emits `chat.turn.stopped` with `reason: turn_stalled`.
- **Spinning**: same tool+arguments fingerprint with the same outcome (error
  class, or result fingerprint on success) 3 times in a row adds a
  `repeated_failure_note`; a 4th ends the turn with an honest `final_text`. A
  changing result resets the streak.

`session.agent_id` is fixed for the turn. Composer prefixes (`agent:<id>`,
`skill:<name>`, `personality:<name>`) are plain text, not routing.
`GoalSource::ChatInline` stamps **spawned** cycles (`delegate_to_agent`,
`handover_to_agent`, `spawn_sub_goal`), run via
`AgentRuntime::trigger_goal_awaitable_with_scope_and_overrides` in Do mode
(`execute_agentic_direct_with_outcome`); they do not answer the ask-mode
message itself.

Live/Realtime **`delegate_to_chat`** and hands-free turns think with the client's
composer engine (`chat_choice` on `session.start`), else `chat.harness_engine`
(Settings → Engines "default for all clients"; changing it switches open
composers via `chat.engine.updated`), with Magician plane tools as hands. GPT-Live-1 is
the spoken mouth; the chat mouth is the brain (not the VibeDev engine picker).

## Structured response presentation

Eligible non-user messages carry a server-derived `ChatMessage.presentation`
sidecar when the canonical output fits V1's envelope
(`magician/src/magician_v2/chat/presentation.rs`, schema
`magician.structured_response`); `plain_text` projects semantic `content`. A
supplied sidecar is kept only if it equals the deterministic projection;
otherwise it is replaced; over-bound output omits it (never truncated).
Artifact targets name a session output or scoped task output, never filesystem
paths. HITL and task-tail cards stay specialized.

## Unified tool-result and turn-context contract

After an authorized tool returns, the shared result runtime applies the
post-dispatch safety guard, materializes one immutable scope-bound raw result,
and derives deterministic model, spoken and display views.
`ChatLlmTranscriptEntry::ToolResultProjected` persists the versioned projection
and call identity; provider history gets only the bounded model value. Legacy
entries hydrate without a destructive migration. A projection or
materialization failure yields one balanced typed result for the original call
id, never a JSON character prefix.

Storage never guesses key names: injected credentials are removed by exact
known-value replacement before materialization; capability metadata alone declares record paths,
priority, atomic groups and narratable fields. An outbound-only guard catches
credential protocol fields and syntax before transport without mutating
evidence; `token` / `secret` / `cookie` / `authorization` fields survive unless
the value is an injected or protocol credential. The runtime
gets a typed `ToolOutcome`; unknown status strings use the rail's typed
fallback; null `error`/`error_code` is not a failure.

Large values page through authenticated
`POST /api/magician/v2/chat/sessions/{id}/results/read`; the opaque reference is
only a locator, and each page rechecks scope, session owner, agent, tool/trust policy, authority
revision, retention, cursor and content hash. Reconstruction v1 uses deterministic preorder `complete_value`, `container` and UTF-8
`string_fragment` units.

Memory and procedure context start concurrently behind the shared staged
coordinator under `agent_surface_runtime.context_retrieval.chat`: fast immutable
memory, hybrid memory and procedures each have their own
completed/empty/error/timed-out state. A slow sibling cannot erase completed
evidence, cancellation clears turn-bound values, and completion order never
changes merge order. Conflicting content for the same source identity, revision and
completeness is rejected as `evidence_revision_conflict`.

### Interrupted tool-history recovery

Native replay validates persisted tool calls as atomic batches before choosing
a continuation anchor. Complete pairs
stay ordered; unmatched calls, duplicate ids and standalone results are omitted
from provider input only, replaced by an interruption marker. With a missing
result and no later trusted boundary, provider-native state from that turn on is
cleared in memory (a newer OpenAI Responses `response_id` cannot continue it).
The first successful final no-tool response stores a server-owned
`tool_protocol_repair_checkpoint`; tool-call responses never become
checkpoints. Durable history is never rewritten.
`make test-provider-replay-live-eval` sends one request per configured family
(OpenAI Responses also probes `response_id` checkpoint/reuse).

## Session Model

Ordinary chat and brainstorming threads may keep several active sessions;
get-or-create reuses only an exact principal/workspace/thread **and agent**
match. Navigation is id-addressed (`?session=<session-id>`): an active sibling
becomes writable; an archived one stays read-only with the prior active session
as the return target.

Records persist `history_lane`: `personal` (`#general` plus user-created) or
`automated` (screen/tab observation, meeting, contextual-writing, thinking-map,
VibeDev, external-contact). The History drawer keeps Sessions/Threads as its
primary switch and filters either by lane with offset pagination; a non-empty
search matches thread names and session titles across both lanes in one
recency-ordered page, each hit tagged with its lane; thread-scoped browsing
ignores the lane. Migration marks only `#general` Personal (seed titles `Wtf`
and `hi i think you have access...`, migration-only); new user-created history
is Personal, product callers stamp Automated. `#general`'s original session
carries `is_default_session`: the store restores it to Active and rejects
archive/delete/move.

`GET /api/magician/v2/chat/sessions` and `GET /api/magician/v2/ui-threads`
accept `history_lane`, `q`, `limit`, `offset` (omit for the legacy full list).
`GET /api/magician/v2/history/search` requires `q` (≤120 Unicode scalars;
offset ≤100,000) merges both lanes and loads full records only for the page. UI-thread startup
migrates the existing table before applying current indexes. Product callers of
`GET /chat/active` / `POST /chat/new` pass `history_lane=automated`; default
`personal`.

**New Chat** never auto-archives on user threads; `screens`, `tabs` and
`vibedev` keep one rolling session per agent
(`chat::storage::thread_rotates_sessions`). Channel is origin, not a partition;
enrollment resolves identity to principal and serializes **under** the write
guard, so two writers cannot land an older snapshot last.

## Runtime Architecture

| Module | Owns |
|---|---|
| `magician/src/magician_v2/chat/service.rs` | `ChatService`, ask-mode orchestration |
| `chat/storage.rs` / `chat/models.rs` | Persistence / models |
| `chat/llm_service.rs` | LLM client |
| `chat/tools_runtime.rs` | Chat-only tools |
| `magician-api/src/chat_api.rs` | HTTP handlers, mounted from `magician-bin/src/main.rs` |

`ChatService` persists the user turn, then runs `process_chat_inline_turn`;
assistant text is a valid terminal reply. The prompt carries a bounded "Recent
Session Files" block (`render_recent_session_files_block`). Sync and streaming
resolve through one authoritative `messages[]` batch.

### Turn tool surface and working set

With a `ScopedCapabilityResolver`, the turn resolves one immutable
registry/schema-index pair in `tokio::task::spawn_blocking`; failures **fail
closed** (no global-registry fallback). Chat, realtime voice and autonomous tasks share a revision-bound
surface-plan cache and working-set store, keyed per session, call, owner frame
and feature mode.

**Per-name identity.** Chat and voice reconcile the loaded set per name: each
loaded tool records a digest of pack and tool
name, canonical schema, pack definition (minus description/guide prose),
result-projection contract and provenance (`app:<installation>`, `embedded`,
`scope`). On an authority-revision change a name survives only if still
authorized with the same digest, so an unrelated skill install or
agent-definition edit keeps the working set while a changed or revoked tool
drops. Bytes outside the pack definition (a script its command runs) are not
hashed. Safe because the set holds names only: every catalog and dispatch
ceiling is re-resolved from the current snapshot.

Without digests, authority **lineage** (registry revision + definition digest)
decides: same lineage keeps authorized names, a new one clears. Autonomous tasks
clear on any revision change. A Chat turn freezes projection inputs (runtime
specs, sorted delegation/handover targets, approval rules, trust digest,
description suffixes) at turn start, so a select's rebuild keeps the turn's
revision (mismatch logs a `[SURFACE-RUNTIME]` warning). Lineage moves only on content: the skill-catalog digest counts only
dirs with `SKILL.md`/`tool_schema.yaml`, an empty computed-capability overlay
equals no catalogs, a failed revision read is labelled `uncached:<digest>`, and
the authority revision hashes canonical JSON (no `HashMap` order). Logs:
`[SURFACE-RUNTIME] chat surface authority revision inputs`,
`[CAPABILITY] scope revision components` (debug), `[WORKING-SET] lineage changed`
(INFO).

**`tool_search`** ([FLAT_LOOP.md](execution/FLAT_LOOP.md) has the handler):

- `tool_search(select:...)` is a solo call committing the selection for the
  **next** iteration; the catalog holds the **merged** working set and the result
  names `unloaded_families` evicted (LRU; re-select or a successful call
  refreshes, on Chat and voice).
- Matches already advertised are marked `"loaded": true`; an all-loaded select
  answers "Already in your tool list: call it now … Do not select it again".
- Realtime voice sends a correlated catalog update and withholds the result
  until acknowledged (compare-and-swap on binding instance, policy, generation).
- A hit on a **granted** tool whose skill failed to load
  (`ScopedCapabilitySnapshot::load_failures`) returns `unavailable` (`name` +
  reason) and `unavailable_note`, not a bare `matches: []`; ungranted failures
  stay hidden. The autonomous-task path does not annotate yet.
- Tools the §4A projection withholds (`tool_search::withheld_from_offer`) are
  never listed as matches or committed: a select of only withheld names returns
  `matches: []`, and select or keyword hits appear under `unavailable` with
  `withheld: true`;
  `annotate_withheld_delegates` adds `delegate_to` (targets granted the pack,
  never the caller) for `delegate_to_agent`.
- Logs: `[SURFACE-RUNTIME] tool_search select committed`; WARN
  `[SURFACE-RUNTIME] loaded tools are missing from the provider tool list`.

**Deferred tools block.** The system prompt lists deferred tools by name
(`native_integration::render_deferred_block`, the block agent runs get) plus "Prefer a tool you have …
over delegating". Being written once per turn, it claims nothing about current
loads: a loaded tool is callable on the **next tool call** and the latest
`tool_search` result is authoritative. Load results open "Loaded and callable
now: `…`" with `callable_now` (`tool_search::stamp_callable_now`), an ordinary
tool result on every provider that leaves the cached prompt prefix alone.

**Alignment holds and confirmation.** A pack sub-run held by preference
alignment makes `fold_chat_pack_outcome` post one answerable
`preference_alignment` card, not a text line
(`escalation_listener::alignment_hold_card`: generic prompt, the ask's input
schema and choices, the pause correlation id; listener and fold both check
`session_has_pause_card`). The tool result carries `pause_state_id` and
`execution_id`, omits the action summary, and says not to re-ask or retry. An owner's yes to chat's own confirmation counts
once, for the next pack call (`preference-alignment.md`).

### Whole-pack tools and the shared browser window

Chat and voice read the **surface tool index**
(`ScopedCapabilitySnapshot::surface_tool_index`,
`flat_loop::build_surface_tool_index`): a multi-primitive pack (`browser`,
`duckdb`, CLI skills with typed actions) is one pack-named tool
(`ToolIndexEntry::runs_as_subrun`) listing its actions. A call mints a
task-backed sub-run that drives the leaves; leaf names like `browser__open` are
never advertised.

- It says to call it ONCE with the complete objective (`command`, `goal`, …).
- `restricted_toolset::project_index_entry` offers the entry as declared; the
  leaf it calls is projected (§4A), gated outward and alignment-checked at
  dispatch. Catalogs, search and promotion share
  `native_catalog::build_index_entry_tool`, so send-only sub-run packs
  (`agentmail-send`, `kapso-whatsapp-send`) stay callable. A non-sub-run
  send-only free-text entry is still withheld.
- `tool_search(select:browser__open,…)` loads `browser`
  (`ToolIndex::pack_for_selected_name`); an empty select says "nothing was
  loaded"; keyword listings carry `how_to_load`.
- A `chat_inline_adapter` pack keeps its leaves. A leaf reaching
  `dispatch_capability_pack` is scoped to its pack with the action in the goal
  (`chat_pack_dispatch_target`, `chat_pack_leaf_goal_text`).

Pack sub-runs share the thread's browser window (`magician-chat-<thread>`, as
delegations do; never `magician-<execution>`), so a later call in the task sees
the page the previous one left. It is recorded in
`browser_run.additional_session_ids` and closed at terminal cleanup. On the
owner's Chrome (CDP) the daemon exits without `Browser.close` and Magicutor's
`DELETE /cdp/threads/<session>` closes only the automation's window. It stays
only when asked (`keep_browser_window_open`, `keep_browser_cdp_connection_alive`)
or paused; the next task on the thread opens a fresh one.

### Public contacts and message content

`FilePublicContactProfileStore` (`chat/public_contact_profile.rs`) persists
per-sender identity/research, exposed by
`GET /api/magician/v2/chat/public-contacts` and
`GET /api/magician/v2/chat/public-chat/status`. Queues and daily budgets persist
in `system/public_chat/runtime_state.json`.

`ChatMessageContent`: `Text` (optional `plan_reply`), `ToolCallExecuted`,
`RichToolResult`, `Attachment`, `TaskStatusUpdate` (`synthesis_pending` /
`speech_tts`), `Escalation`, `EscalationResolved`. No `PackProgress`.

## Tool Model

Task controls (execution-native agentic surface): `list_tasks`; compiled
`get_task_details` (`execution/embedded_pack_defs/get_task_details.yaml`) and
richer `get_task_details_for_chat`; `create_task` (always persistent; optional
`reference_task_ids`, `TaskSchedule` JSON; `dispatch_create_task` ignores LLM
`agent_id` for the thread's personal agent); `run_task` / `stop_task` /
`update_task` / `delete_task`; `preview_monitor` / `create_monitor` /
`update_monitor` (`create_monitor` needs `preview_fingerprint`;
[recurring-monitors.md](recurring-monitors.md)); `spawn_sub_goal`,
`delegate_to_agent`, `handover_to_agent`, `find_agents_for_capability`;
`create_dashboard` / `unpublish_dashboard` (under
`execution/embedded_pack_defs/`, not `capability_templates/`).

**Adaptive-profile escalation.** With an adaptive composite profile,
`process_chat_inline_turn` injects `request_thinking_mode`; the runtime
intercepts it before persistence, swaps `fast_profile` → `thinking_profile`, and removes the tool
(once per turn). See [chat-profile-routing.md](chat-profile-routing.md).

**Capability packs.** `dispatch_capability_pack` creates an `Internal` V3 task
(`sync_mode: Await`, `created_by: "chat_pack_dispatch"`, `chat_session_id`),
subscribing **before** `activate_execution` so the first `running` card lands.
Pack `ImplementationType`: `Composite` / `Compiled` / `Primitive` / `Command`
(no JavaScript). The sole chat-inline adapter, `ChatInlineAdapter::TutorScreenDraw`,
is a five-second local host gateway or realtime-client event, never a task.

**Chat-runtime tools** (`ChatRuntimeTool`, `tools_runtime.rs`), chat surface
only: session/thread archive-delete-switch, `get_current_chat_context`,
`record_chat_teaching_feedback`, `get_task_details_for_chat`,
`subscribe_to_task_for_chat`, `describe_agents_for_chat`, `list_tools_for_chat`,
`read_result`, plus Tutor/App Copilot tools (`start_tutor_run`, …) when the lane
keeps them. Untrusted agents get no session/thread or spawn/delegate/handover
controls. `record_chat_teaching_feedback` reads `target_tier` unscoped
(`learning::teaching::unscoped_tier`): `user.knowledge` names `knowledge`.

**Heavy work leaves the run's frame.** A run's decision chain already uses
~1.4 MiB of the worker before a tool runs, so synchronous work done there —
re-parsing the config, hashing skill catalogs — runs on the scheduler root
(`run_on_agentic_scheduler_root`): the owner profile load and the scope
capability snapshot refresh both do. Inline, the snapshot refresh
overflowed the worker whenever a settings write invalidated its cache
(god mode toggled mid-run, 2026-10-15), and resumed runs crash-looped.

**Stack frames, not bigger stacks.** Chat runs on the default 2 MiB worker
(`runtime_boundary` refuses to raise it). Awaits go through `off_frame!`; the
widest futures are built boxed (`resume_agentic_execution_with_scope_boxed`,
`harness_tool_future`). `execution::stack_headroom::with_stack_headroom` is only
for deep execution work (worker-driver phases, child runs, app-workflow launch
admission, terminal success gate).
`make check-stack-frames` (`scripts/stack_frame_budget.py`) fails when a listed
function (`resume_agentic_execution_with_scope_inner`,
`process_chat_inline_turn_inner`, `dispatch_chat_tool_call`, …) exceeds budget.
See [runtime-async-stack-boundaries.md](runtime-async-stack-boundaries.md).

**Dispatch chokepoint.** Every chat tool call goes through
`dispatch_chat_tool_call`, which first revalidates the authorization revision,
name ceiling (`allowed_tool_names`), trust policy, `denied_tool_params` and
structural approval; a change returns `ChatPolicySnapshotChanged` and runs
nothing. Compiled calls from `reviewed` / `untrusted` agents, and `builtin` /
`local` calls that match an approval rule or are outward (argument-aware), enter
the task-backed executor; the remaining fast path never reaches outward
disclosure, recipient compliance, capture posture or the effect ledger. Memory-bridge parameters overwrite `agent_id` with
the bound chat agent.

No `ChatToolRegistry`, YAML chat-tool path or `tool_call_proposal`
continuation exists; domain work is an agent capability.

## Composer UX and reference picker

`FloatingComposer.svelte`: Enter or Cmd/Ctrl+Enter sends (queues while busy,
shown by `QueueInspector`), Shift+Enter is a newline; Stop, shown only while a
turn is in flight, calls
`DELETE /api/magician/v2/chat/sessions/{id}/run`.

The `@` picker only inserts text: `@agent` → `agent:<id>` (session agent plus
delegates); `@skill`/`@tool` →
`skill:<name>` (delegate-owned: `skill:<name> via agent:<id>`); `@personality` →
`personality:<name>`. Source:
`GET /api/magician/v2/chat/sessions/{id}/reference-catalog`; a missing session
or catalog failure **fails closed** (no global-directory fallback).
Selection never calls `activate_skill` or a control API.

## Transport bus and per-chat-turn events

All progress/chat/agent events flow over `RuntimeTransportBroadcaster`; sinks
`subscribe()` synchronously **before** `tokio::spawn`.

| Sink | Role |
|---|---|
| `ChatStoreSink` | Sole `chat_store` writer; projects `ChatMessageReceived`; `ProgressEvent` → `ChatChannel::deliver` `TaskStatusUpdate` cards |
| `ChatTurnEventSink` | Appends `<scope>/ui/chat_turn_events/<chat_turn_id>.jsonl` by `(chat_turn_id, principal, workspace)`, broadcasts for SSE; recovers missing ids via `ChatFanoutResolver::resolve_turns` |
| Persistence sink | Per-scope `events.jsonl` |
| `EscalationListener`, `PlanningListener` | Emit chat messages onto the bus (never write the store) |

`chat_fanout_by_task` copies delegate events under the chat agent's identity
(`chat_delivery_kind: "inline_delegate"`), rewriting `tool.call.*` `call_id` to
`delegate-{execution_id}/{orig_call_id}`.

`GET .../turns/{cid}/events` returns the last 1 MiB
(`CHAT_TURN_EVENTS_TAIL_MAX_BYTES`), at most `limit` newest rows (default 50),
`truncated`, and whole-file own-call `usage`
([magician-api](../magician-api/README.md)); `.../events/stream` replays disk
then live, deduped by `event_id`. The old `/api/magician/v3/events?chat_turn_id=…`
filter is gone.

Delegate/handover subscriptions carry `chat_subscription_owner_released: "true"`:
the router keeps them past the execution's terminal `StatusChanged` until their
owner releases them
(`WatcherCleanupGuard`, the turn's teardown, or a declining watcher); boot
pruning drops survivors. Terminal catch-up emits
`chat.delegate.status_changed` then the card under the real turn id
([v2-websocket-events.md](v2-websocket-events.md#chat-delegate-terminal-chatdelegatestatus_changed-agentevent)).

`chatTurnEventsStore` reconnects with 1s→30s backoff; a 30s REST watchdog
reconciles subscriptions idle 60s; `liveTurnId` keeps the spawning turn
subscribed while the delegate lives. Inspect opens `TaskPanelDrawer` via
`GET /api/magician/v3/tasks/{id}/execution-panel?execution_id=…` (both ids
required; otherwise `Show all`). Chat-inline LLM calls emit
`RuntimeTransportEvent::LLMResponseReceived` with `capability="chat.inline"`
([duckdb-analytics.md](duckdb-analytics.md#emit-site-coverage)).

## HITL in the chat surface

HITL during a turn turns the typing bubble into a "Waiting on you — click to
respond" pill (`pendingHitlByChatTurnId` in `ChatPanel.svelte`), opening the
AttentionBar / `/attention` modal; answers go to
`POST /api/magician/v2/hitl/{cid}/respond`. `EscalationListener`, started at
boot in `magician-bin/src/main.rs`, projects canonical HITL events into the
owning active session as `Escalation` / `EscalationResolved` cards (one per `pause_state_id`; `user_request` resolutions only when the session
holds the card). `ChatChannel::deliver` skips `hitl.*` to avoid duplicate rows.
Service-backed escalations respond via
`/api/magician/v2/user-requests/{request_id}/respond`; execution-owned pauses
via `/api/magician/v2/executions/{execution_id}/execution/agentic-resume`. Full
contract: [HITL / Attention](hitl-attention.md).

## Chat-run Cancellation and Pending-Message Queue

`ChatService::active_chat_runs: Arc<DashMap<String, ActiveChatRun>>` holds one
`{ token: CancellationToken, owner_id }` per in-flight turn, registered
atomically (`try_register_active_chat_run`) and cleared by an RAII
`ActiveChatRunGuard`.

`DELETE /api/magician/v2/chat/sessions/{id}/run` → `ChatService::cancel_chat_run`
removes the slot, signals the token, terminally cancels the scoped tutor run,
and best-effort preempts a pending delegated UI action; the turn returns
`ChatResponse { cancelled: true, assistant_message: None, .. }`. `run_task` /
`create_task` do **not** cascade.

| Site | Cancellation behavior |
|---|---|
| `process_chat_inline_turn` | Races the LLM call against `cancel_token.cancelled()` (drop sends `RST_STREAM`); checks each iteration; after persisting an in-flight result, skips remaining calls |
| `process_message_inner`, `process_message_streaming_with_mode` | Check **before** `persist_user_turn` |
| Delegate/handover | Watchdog (5s `get_task` poll, 3-hour deadline) → `ArtifactV2Service::cancel_execution_by_id` |
| Pack dispatch | Token → `PrimitiveExecCtx::cancellation_token` |
| SSE disconnect | `SseDisconnectCancelGuard` `Drop` → `cancel_chat_run` unless the response was written |

**Tutor.** `POST /api/magician/v2/chat/sessions/{id}/tutor/cancel` is idempotent within
the resolved scope,
works after the session is gone and runs before session delete; overlay
teardown follows `tutor.run.failed` / `tutor.run.completed` (`idle`). See
[personal-tutor.md](personal-tutor.md).

A message arriving during an active turn is enqueued in
`pending_messages: DashMap<String, VecDeque<QueuedMessage>>`
(`MAX_QUEUED_PER_SESSION = 5`, drop-oldest). After each successful turn,
handlers `tokio::spawn(svc.drain_pending_queue(sid))` when
`pending_queue_depth > 0`. In-memory only; a supervisor restart drops it.
REST: `GET/DELETE .../queue`, `DELETE .../queue/{message_id}`. UI:
`QueueInspector.svelte`. Bot SDK: `/stop`, `/queue`, `/clearqueue`. Decisions:
`docs/archive/plans/2026-05-16-chat-message-queue-and-control-commands.md`.
Streaming: [chat-sse-streaming.md](chat-sse-streaming.md).

## Outer-loop cleanup

LLM and transcript-persistence errors go to `deferred_error` and `break`, so the
cleanup tail (`unsubscribe_turn_subscriptions`, `unregister_chat_fanout_for_task`)
always runs first. With an error, the tail persists (best effort) a
`ChatLlmTranscriptEntry::AssistantTurn` starting `[chat-inline] turn failed:`
with `provider_state: None`, so the OpenAI Responses anchor does not pin the
prior `response_id`. Each turn, after `get_llm_history`,
`first_chain_poisoning_index` finds consecutive user entries or a failure
marker, and `heal_poisoned_chain` inserts synthetic assistant markers and strips
`provider_state` from that index on, in memory only.

## Delegation

Default `GoalSource::ChatInline` delegation creates an `Internal` task
(`created_by: "chat_delegate"`, `sync_mode: Deferred`); `track_as_task: true`
makes it `Persistent` (`created_by: "chat_delegate_tracked"`); cycle creation,
scope binding and fan-out are identical. A multi-target batch is prepared
behind one launch gate and released only after every child is admitted.

`personality_mode` on `delegate_to_agent` / `handover_to_agent`
(`ChatService::resolve_personality_directive`): omitted / `default` / `none` /
`off` / `inherit` → none; `current` → caller's `personality_profile`; an
installed personality-mode skill → that preset for one cycle. It rides
`GoalTaskOptions.personality_directive`, prepended at LLM dispatch (never in
`task.title` / `task.description`); non-noop modes tag the task `voice:<mode>`.

`find_agents_for_capability` returns reachable agents matching owned skill/pack or id/name/alias
(`web_research` finds `web-researcher`). A target's `required_capability`
mismatch returns retryable `CapabilityNotOwned`. Multi-capability work uses
ordered rounds (B gets A's artifact ids).
Chat-inline cycles skip Plan mode, so `GET /tasks/{id}/plan` returns 404.

## Planning replies in chat

Only `eliciting` questions and `draft` plans sit by the composer. **Reply**
sends `task_id` / `question_id`; `ChatService::resolve_plan_reply_target`
re-reads the scoped task plan and verifies the task is in the session's UI thread and the question is pending. The user text carries a
`plan_reply` envelope. Plan-mode without a target means a new plan; ask-mode
with plan target metadata is rejected.

## Media and outputs

Attachments upload to the session outputs store first; attachment-aware
(including attachment-only) sends persist one
canonical `UserTurn` plus display messages.
`GET /api/magician/v2/chat/sessions/{id}/outputs/{path}` serves HTML / PDF / SVG
/ text / audio / video inline (`task_api_v3::infer_safe_inline_media_type`).
`POST .../outputs/open-folder` and `.../open-file` accept scoped roots or paths
the session referenced; `open-file` refuses unsafe extensions.

Media packs: `image-generation`, `video-generation-via-veo`,
`gif-search-via-klipy`, `meme-generation-via-imgflip`. Image-MIME artifacts set
`prompt_image: true` for multi-turn replay; prompt multimodal input is `image/*` only. Anthropic replays native `tool_use`, image-bearing `tool_result` and signed
thinking. OpenAI Responses and xAI Grok (`provider: xai`, a Responses-family
replay via `is_openai_responses_family`, vision gated by
`XaiProvider::model_supports_vision`) continue via `previous_response_id`.
Clearing messages clears `llm_history`. MiniMax is text-only for attachments.
`GET /chat/profiles` lists rich-chat-safe profiles, flagging image input.

Retention follows `GoalTaskOptions.lifecycle` (`internal` swept with the
session, `persistent` kept; `track_as_task` promotes), not
`GoalSource::ChatInline`; `ChatChannel::deliver` copies outputs first.

## Server-Published Invoke-Grammar Catalog

`GET /api/magician/v2/chat/invoke-grammar` (stateless, read-only) serves tutor,
app_copilot, brainstorm and vibedev grammar: `version` (schema; `1`), `etag`
(sha256 of lanes), `leading_invoke_required` (invokes must open the turn), and `lanes.<lane>` (`markers`,
`spoken_phrases`, `quick_flags`, vibedev `spoken_shape`). Built by
`chat::invoke_catalog::invoke_grammar_catalog()` from
`TUTOR_MARKER_INVOKE_WORDS`, `APP_COPILOT_MARKER_INVOKE_WORDS`,
`VIBEDEV_MARKER_INVOKE_WORDS`, `TUTOR_QUICK_FLAG`, `VIBEDEV_DISCUSS_FLAG`;
checked by `magician/tests/invoke_grammar_agreement.rs`. Additive-only.
Brainstorm has no spoken phrases (`hey brainstorm` is client-side). Plain
`@tutor` / `@copilot` / `@brainstorm` text is not authorization.

## Conversational lane seam

Lanes register in `magician_v2/chat/lane_seam.rs`, not new arms in
`chat/service.rs`. `ChatLane` declares `feature_mode`, `admission_surface`,
`matches_session` (`LaneSessionProbe`), `hot_chat_tools` (`LaneHotToolsProbe`),
and fail-closed `keeps_tutor_runtime_tools`,
`injects_personal_tutor_instructions`, `promotes_tutor_screen_draw`,
`narrow_delegation_targets`, `same_lane_invoke`,
`participates_in_narration_approval_sweep`. `registered_lanes()` order:
Brainstorm, VibeDev, Tutor, App Copilot. `authenticated_product_lane` decides
admission; `hot_chat_tools_for_feature_mode` supplies hot tools (voice adds
`list_tasks`). The seam holds `BRAINSTORM_FACILITATION_AGENT_ID`,
`THINKING_MAP_THREAD_ID`, `THINKING_MAP_PRODUCT_SOURCE_KEY`.

Tutor/App Copilot admit on a leading `@tutor`/`@copilot` plus a recognized
product-source label. VibeDev admission stays in the service, ahead of realtime
voice, deciding via `vibedev::rail::{rail_admits_turn, rail_turn_lane,
vibedev_rail_reply_key}`; `@vibedev` is the only chat-turn invoke that creates a task instead of replying
([vibedev-rail.md](vibedev-rail.md)). Active-run continuation, tutor-quick
allowlists, screen-draw grant widening and voice takeover also stay in the
service. Oracles: `magician/tests/lane_admission_oracles.rs`.

## Context Retrieval Latency

Memory and procedure retrieval share one semantic query (no ownership, scope or
audit identity). In-flight embeddings coalesce; a success is kept 25 ms (key: endpoint, model, dimensions, embedding context, batch-token
contract, exact query); failures never.

Each turn logs `[CHAT-RETRIEVAL]` with `memory_ms`, `memory_index_query_ms`,
`memory_user_render_ms`, `memory_agent_render_ms`, `procedures_ms`,
`concurrent_wall_ms` (critical path) and `overlap_saved_ms`. User and agent
memory share one hybrid query and render on separate cancellation-owned Tokio
tasks, assembled user-then-agent. Prompt usage persistence is a background
worker after selection.

`make benchmark-chat-context-retrieval` (args via
`CHAT_CONTEXT_RETRIEVAL_EVAL_ARGS`; reports in
`coverage/evals/chat-context-retrieval/`); provider-free
`make test-chat-context-retrieval-eval`; resident-model
`make test-chat-context-retrieval-live-eval` (in `make test-live-evals`).

## User surfaces

Routes: `/chat`, `/t/[name]`, HUD `/hud`, shell bubble/overlay. Web chat groups
consecutive `task_status_update` messages per task wave (display-only); channels get the raw
stream. MUIJ `agent.ui.*` deltas stay off the transcript.
`DELETE .../messages[/{message_id}]` keeps the session; bulk clear walks every
segment. Web chat and bots share this API; channel delivery:
[Consumer Channels](../../consumer_channels.md).

Mounted under `/api/magician/v2`: `GET /chat/active|profiles|sessions[/{id}]`,
`POST /chat/new`, `GET .../reference-catalog`, `POST .../messages` and
`.../messages/stream`, `DELETE .../run`, queue + tailed-task CRUD, turn `events`
+ `events/stream`, `POST .../results/read`, session `outputs` +
`open-folder`/`open-file`, `POST .../tutor/cancel`, `GET /chat/invoke-grammar`,
public-contact status/list, `POST /chat/enroll` and `/enroll/approve`.

## Latency posture (chat-only)

`chat_outer_loop_system` v0.0.7's **Response shape** section biases toward ~3–5
sentences unless depth is asked for, matched by `max_output_tokens` on
`chat-gptterra-responses-vision-toolsauto-fast` (4,096) and
`chat-gptterra-responses-vision-toolsauto-thinking` (8,192). Agentic, memory,
mining and planning profiles keep their budgets.

Loom (`brainstorm-facilitator`) resolves `brainstorm_facilitation` on
`chat-gpt6luna-responses-vision-toolsauto-fast`, ignores generic overrides and
permits only OpenAI `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-6-luna` (and their
suffixed variants) or `gpt-6.1-sol` (`brainstorm_facilitation_model_is_allowed`);
any other mapping falls back to `chat-gpt61sol-responses-vision-toolsauto-fast`.

## Current boundaries

- Chat storage is separate from task threads and planning.
- Realtime voice authority is never inferred from `voice_origin`
  (presentation/TTS only); the server voice bridge mints
  `authenticated_realtime_voice` and requires the session owner to match the
  agent.
- Public Envoy chat clears the native catalogue; Envoy's definition also denies
  `search_memory`.
- Chat omits autonomous-only `yield` / `need_user_input`.
- DuckDB `spawn_blocking` cannot be cancelled by future drop.

## Accept-in-scope is a per-scope preference, not per-session state

A turn carries `ChatMessageMode` (`ask` / `accept_in_scope` / `plan`);
`ChatService` mirrors the accept bit into `accept_in_scope_sessions` for deep
pack sub-executions. That map is one-turn, in-memory carry-through: each send
overwrites it and `clear_chat_session_state` must remove it, or a stale bit
outlives the session. The durable preference is
`UiPreferences::composer_permission_mode` (per principal + workspace, default
and fallback `ask`); sibling `UiPreferences::theme` defaults and normalises to `sorbet`. See
[composer permission mode](../unified-ui/composer-permission-mode.md) (what it
waives is narrower than the name) and
[the endpoint contract](../magician-api/ui-preferences-endpoint.md).

Related: [plane.md](plane.md), [chat-profile-routing.md](chat-profile-routing.md),
[chat-sse-streaming.md](chat-sse-streaming.md), [hitl-attention.md](hitl-attention.md),
[personal-tutor.md](personal-tutor.md), [vibedev-rail.md](vibedev-rail.md),
[envoy-agent.md](envoy-agent.md).
