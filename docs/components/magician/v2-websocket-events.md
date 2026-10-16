# Magician V2 WebSocket Events Reference

Contract for `RuntimeTransportEvent`, the tagged enum in
`magician/src/magician_v2/realtime_events.rs` (114 variants), delivered by
`GET /api/magician/v2/realtime/ws` and `GET /api/magician/v3/events` and
stored on disk in a mapped GAUI form. Wire:
`#[serde(tag = "event_type", content = "data")]` (outer `event_type` = the
PascalCase variant, payload under `data`). Taxonomy lives in
`magician-event-taxonomy` (`EVENT_TAXONOMY_TABLE` + `GAUI_EVENT_TAXONOMY`);
`make event-taxonomy-codegen` regenerates the TypeScript mirror and
`make event-taxonomy-check` fails the build on drift.

`UserRequestPending` / `UserRequestResolved`, `ClarificationQueued` /
`ClarificationResponseReceived` and `V3PlanningClarificationNeeded` /
`V3PlanningClarificationResolved` are not on the enum; those surfaces emit only
`HitlRequested` / `HitlResolved`.

## Transports

### WebSocket — `GET /api/magician/v2/realtime/ws`

Handler `magician-api/src/websocket_handler.rs`; local URL
`ws://127.0.0.1:3002/api/magician/v2/realtime/ws`. Bearer-scoped: browsers
offer `magician-events-v2` then the auth-only `magician-bearer.<token>`
protocol; the handler selects `magician-events-v2` and never echoes the
credential. Native clients may send `Authorization` instead.

Query (`WsConnectParams`):

- `execution_id`: re-emit that execution's pending pauses on connect.
- `agent_id`: also re-emit `FullPauseStore::get_pending_for_agent()`.
- `supports_structured_presentation`: false/omitted strips
  `ChatMessageReceived.message.presentation`.
- `resume_epoch` / `resume_seq`: previous `epoch` and last processed `seq`
  (u64; non-numeric → `400`).

`validate_origin`: a missing Origin is allowed; a present one must match
`localhost` / `127.0.0.1` / `[::1]` or the request Host, else
`403 origin_mismatch`. Invalid `execution_id` / `agent_id` → `400`; reconnect
during legacy pause-index repair → `503` `legacy_pause_index_repair_pending`.

The session:

- Sends `{"type":"hub.hello","epoch","seq","resumed"}` first (then
  `hub.resync` if a requested resume fails), then a `Heartbeat` as the ready
  signal (no Connected event).
- Re-emits pending pauses as slim markers (`AgenticWaitingForUser`,
  `AgenticWaitingForConfirmation`, `AgenticMaxIterationsReached`, or
  `ExecutionPaused` for a manual pause); dedupe on `pause_state_id`. Prompt
  fields are only on `HitlRequested`.
- Replays the resumed gap, then forwards what `event_visible_to_scope` accepts
  for the bearer principal/workspace. App-owner notification lifecycle events
  never reach this socket.
- Does **not** filter by taxonomy (`category` / `severity` / `user_relevant`
  are `/events`-only); frames carry `categories` and `user_relevant` labels.
- Pings every 5s; client timeout 30s; inbound frames ≤ 64 KiB.
- Never closes for backpressure. Mailbox: 1024. A lagged receiver or full
  mailbox sends one `{"type":"hub.resync","reason":"lagged"}` and drops events
  until it is delivered. (`GET /v3/events` emits `__events_lagged__`.)

#### Event hub: sequence, resume, resync

Contract: `docs/archive/plans/2026-10-01-single-event-hub.md`.

`EventHub` (`realtime_events/event_hub.rs`) stamps every event reaching the
broadcast ring (`broadcast_transport_event`) with a monotonic `seq` from 1 and
keeps the last 8192 (`EVENT_HUB_REPLAY_CAPACITY`) for replay. `epoch` is a UUID
per broadcaster (one per process: a boot id); a new epoch is a new `seq`
space. Only this socket uses the sequenced channel. Event frames keep
`event_type` and `data` and add `seq` (u64, assigned once), `epoch`,
`categories` (matched `/v3/events` category tokens; today one), `user_relevant`
and `refs` (`{task_id?, execution_id?, chat_turn_id?, agent_id?, session_id?}`,
absent keys omitted). Keys are alphabetical (`data` before `event_type`).
Pause re-emits and the connect `Heartbeat` carry no `seq`.

Labels come from `classify_transport_event`
(`realtime_events/event_classifier.rs`), the `/v3/events` filter: the unwrapped
type (`data.event.event_type` for `AgentEvent`) in the shared table, else the
variant's taxonomy; an unrecognized `AgentEvent` type gets the catch-all
(`agent`, `user_relevant:false`), which `/v3/events` soft-passes. `refs` read
the `AgentEvent` payload, envelope, variant fields, then a nested chat
`message`; `session_id` also accepts `chat_session_id`.

Control frames use `type`, never `event_type`:

| Frame | When |
|-------|------|
| `{"type":"hub.hello","epoch","seq","resumed"}` | First frame. `seq` = hub head at subscribe; `resumed` true only if a requested resume succeeded |
| `{"type":"hub.resync","reason":"gap"}` | `resume_seq` older than the ring or newer than the head, or `resume_epoch` without `resume_seq` |
| `{"type":"hub.resync","reason":"epoch"}` | `resume_epoch` is not this process's, or `resume_seq` without `resume_epoch` |
| `{"type":"hub.resync","reason":"lagged"}` | Live delivery fell behind (broadcast lag or full mailbox) |

Resume (`resume_seq=n`): a matching epoch and a ring holding `n + 1` (or `n` at
the head) replays every scope-visible event with `seq > n`, then goes live.
Subscribe and ring snapshot share the publish lock and the session dedupes by
`seq`, so nothing is lost or doubled. No resume params → `resumed:false`, no
resync.

Inbound client JSON (`type` discriminator):

- `agent.ui.snapshot_request` `{ agent_id }` → `agent.ui.snapshot` or
  `agent.ui.snapshot_error`.
- `ui.interaction` `{ agent_id, component_id, goal_id?, trigger?, request_id? }`
  → `ui.interaction.ack` / `ui.interaction.error`; 1 trigger / 5s per
  `(agent_id, component_id)`.
- Missing `type` → protocol `error` frame; unknown `type` → dropped.

### HTTP — `GET /api/magician/v3/events`

NDJSON (`application/x-ndjson`); query, backfill, the 50k scan cap
(`__events_partial__`) and live lag (`__events_lagged__`) are specified in
[`v2-api-guide.md`](./v2-api-guide.md#unified-event-stream--get-apimagicianv3events).
Query (`EventsQuery`): `workspace`, `execution_id`, `task_id`, `agent_id`,
`since`, `before`, `limit` (default `EVENTS_RETENTION_MIN_COUNT` = 2000),
`category`, `severity`, `user_relevant`, `event_type` (substring), `search`,
`backfill_only`. Backfill: `execution_id` + `task_id` both → the canonical
log; else the per-scope log plus recent per-execution files, default window
24h when `since` is omitted. Lag row:
`{"event_type":"__events_lagged__","timestamp_ms","skipped","message"}`.
Taxonomy filters use `RuntimeTransportEvent::taxonomy()` but
unwrap `AgentEvent` (`data.event.event_type` in `GAUI_EVENT_TAXONOMY`), so
`tool.call.*` / `reasoning.*` / `agent.update` filter as their inner type.
`task.budget.updated` is `execution / info / user_relevant false`: the
execution panel refreshes; chat, feed, webhook and external bots stay quiet
(not a task-card status change).

`POST /api/magician/v3/events/debug-emit` (always enabled) injects a synthetic
`RuntimeTransportEvent` via `emit_transport_only`, live only.

## Wire format and AgentEvent unwrap

`AgentEvent` wraps an `AgentEventEnvelope` whose `event_type` is a GAUI string
(`reasoning.start`, `tool.call.finished`, `agent.update`, …):

```json
{"event_type":"AgentEvent","data":{"event":{"event_type":"reasoning.start","agent_id":"…","payload":{…},"timestamp":…}}}
```

`/events` (`unwrap_event_type`) filters on the inner string; on the WebSocket
match `data.event.event_type`.

## Taxonomy

Two tables, in lockstep with the enum: `EVENT_TAXONOMY_TABLE` (PascalCase
variant → category, severity, `user_relevant` via
`RuntimeTransportEvent::taxonomy()`) and `GAUI_EVENT_TAXONOMY` (dotted string
such as `hitl.requested`, `reasoning.start`; for canonical `events.jsonl` rows
and inner `AgentEvent` envelopes). A new variant needs the same `taxonomies!`
line in `realtime_events.rs` (exhaustive `taxonomy_lookup`) and
`magician-event-taxonomy/src/lib.rs`.

- `EventCategory`: `pipeline`, `plan`, `tool`, `slot`, `clarification`,
  `execution`, `llm`, `agentic`, `hitl`, `agent`, `task`, `feed`,
  `observability`, `media`, `activity`.
- `EventSeverity`: `info`, `warn`, `error`, `decision`, `attention`.

`AgenticWaitingForUser` / `AgenticWaitingForConfirmation` are **Hitl /
attention / user_relevant**. `ChatMessageReceived`, `ProgressEvent`,
`ShellOutputChunk` and `InteractivePtyChunk` are Observability (the
catch-all).

## Scope visibility

`event_visible_to_scope` (`realtime_events.rs`) is fail-closed and
exhaustive over variants:

- Explicit `principal` / `workspace` → that pair only; missing scope on most
  variants → only `system` / `system` consumers.
- `Heartbeat` → every connection.
- `ProgressEvent` / `FeedItemCreated` → inner `ProgressMessage` / `FeedItem`
  scope.
- `ThinkingMapUpdated` / `ThinkingMapInterpretProgress` /
  `DecisionAccountingGap` / `DecisionShadowAgreement` → required scope,
  strict equality, no system bucket.
- `AgentEvent` → envelope scope, else payload `principal`/`workspace`, else
  `system`/`system`.

`RuntimeTransportBroadcaster::emit` / `emit_transport_only` run
`enrich_scope_if_registered`: a `HitlRequested` / `HitlResolved` with a
registered `execution_id` but no principal/workspace/task_id is filled from
`CanonicalEventScope` at fan-out.

## Event persistence + retention

### Per-execution canonical log

`…/scopes/{principal}/{workspace}/tasks/{task_id}/executions/{exec_id}/events.jsonl`:
`FilesystemRuntimeEventSink` writes all 117 `ArtifactV2EventType` variants
under dotted names (`llm.requested`, `tool.succeeded`,
`agentic.iteration_started`, `hitl.requested`, …), mapped by
`map_v2_realtime_event` (`artifact_v2/events.rs`; `HitlRequested` /
`HitlResolved` → `hitl.requested` / `hitl.resolved`). Never time-evicted;
lives as long as the execution directory. Read by `/events` when
`execution_id` and `task_id` are both pinned (no implicit `since` floor).

- `events.jsonl.commit` (≤ 32 bytes) holds the committed length; readers see
  only that prefix; the next writer truncates a failed write's suffix. Marker
  visible but directory sync and retry failed → `CommitUncertain`: never retry
  it as an ordinary failure (readers may already see the event).
- Ingress counts encoded bytes before queueing; records over 64 MiB
  (`MAX_CANONICAL_EVENT_RECORD_BYTES`) are dropped.
- `ref_ids.output_ids` = payload `output_id` then `source_output_ids`,
  first-seen, max 16,384 (`MAX_CANONICAL_EVENT_OUTPUT_REF_IDS`), else the
  event is rejected.

### Per-scope transport event log

`…/scopes/{principal}/{workspace}/events.jsonl`, written by
`transport_log::spawn_workspace_event_log_writer` (one subscriber; capacity
`DEFAULT_RUNTIME_TRANSPORT_CAPACITY` = 8192). Captures broadcaster-only events
(`FeedItemCreated`, `ChatMessageReceived`, `ExecutionPanelDelta`, `AgentEvent`
envelopes, …), routed by `event_visible_to_scope` (missing scope →
`system/system`). Path components are sanitized (alphanumeric + `_-`,
length-bounded); unsafe values go to `_quarantine/_quarantine`. Cross-scope
backfill reads it **without** re-applying `since`.

Retention (hard-coded): `EVENTS_RETENTION_WINDOW_MS` (24h),
`EVENTS_RETENTION_MIN_COUNT` (2000), `COMPACTION_INTERVAL` (5 minutes). The
compactor keeps `max(events_in_24h, EVENTS_RETENTION_MIN_COUNT)` newest rows
(tmp + rename; a per-log mutex serializes append and compact). Files ≤
`EVENTS_RETENTION_MIN_COUNT * 16` bytes are skipped; larger ones stream. Opens
retry up to 8 times on `EMFILE` / `ENFILE` (25 ms doubling, capped at 400 ms),
then return the error. On `RecvError::Lagged` the writer appends
`__transport_log_lagged__` (`skipped`, `timestamp_ms`, `message`) to
`system/system` and logs at `warn!`.

### Frontend dedup

`EventStreamCard` dedupes on the serialized NDJSON line: identical live and
transport-log copies collapse (`serialize_event_if_passes` keeps them
byte-identical); a dotted `ArtifactV2EventType` row and its PascalCase twin do
not.

## Event categories

Counts are live `EVENT_TAXONOMY_TABLE` rows (114; retired HITL aliases
excluded): [Pipeline](#pipeline) 11, [Plan](#plan) 8, [Tool
matching](#tool-matching) 2, [Slot](#slot-extraction) 14,
[Clarification](#clarification) 3, [Execution](#execution-lifecycle) 16,
[LLM](#llm-analysis) 8, [Agentic](#agentic-execution) 19,
[HITL](#human-in-the-loop-canonical) 7, [Agent](#agent-lifecycle) 5,
[Task](#task-crud) 3, [Feed](#feed) 4, [Observability](#observability) 8,
[Activity](#activity) 6.

Unless noted, variants carry optional `principal` / `workspace` and
`timestamp: i64` (unix ms); absent options are omitted
(`skip_serializing_if = Option::is_none`). In the lists below, "ids" means
`execution_id` + `correlation_id`.

## Pipeline

- **MessageProcessingStarted**: ids, `turn_id`.
- **QueryAnalysisCompleted**: ids, `complexity_score: f64`, `intent`,
  `categories: string[]`.
- **StrategySelected**: ids, `strategy: StrategyType`, `confidence`, `reason`.
- **ExplorationProgress**: ids, `nodes_explored`, `current_depth`,
  `best_score`, `current_task`, `progress_percent`.
- **MessageCompleted**: ids, `turn_id`, `response`, `exploration_summary?`.
- **ProcessingError**: ids, `error_message`, `error_type`.
- **PipelineStarted / PipelineCompleted / PipelineFailed** (ask-loop planning
  pipeline): `workflow_id`, `chain_id`; Started + `max_iterations`;
  Completed/Failed + `steps_executed`; Failed + `reason`.
- **PipelineStepStarted / PipelineStepCompleted**: `workflow_id`, `step_id`,
  `agent_id`; Completed + `outcome_kind`.

## Plan

- **AtomicPlanOutlineStarted**: ids, `total_atomic_tools`.
- **AtomicPlanOutlineCompleted**: ids, `goals_count`, `confidence`.
- **AtomicPlanExpansionStarted**: ids, `goals_from_outline`.
- **AtomicPlanGenerated**: ids, `turn_id`, `plan_graph: PlanGraph`,
  `validation_status` (`validating` / `valid` / `retrying`), `attempt_number`.
- **V3PlanningStarted / V3PlanningProgress / V3PlanningCompleted /
  V3PlanningFailed** (task-scoped planner): required `principal`, `workspace`,
  `task_id`, `task_title`, `agent_id`, `plan_id`, `ui_thread_id`; Progress +
  `phase`, `detail?`; Failed + `error`. Planning questions ride
  `HitlRequested { source: "clarification" }`, not a V3PlanningClarification
  variant.

## Tool matching

- **ToolMatchingTierStarted**: ids, `tier_number` (0–4), `tier_name` (Category
  Pre-Filter / Rule-Based / Semantic / Candidate Selection / LLM Evaluation),
  `description`.
- **ToolMatchingTierCompleted**: ids, `candidates_count`, `duration_ms`,
  `top_candidates: TierCandidate[]` (`tool_name`, `score`, `category`).

## Slot extraction

- **SlotExtractionStarted**: ids, `message_length`.
- **SlotExtracted**: ids, `slot_id`, `slot_type`, `confidence`.
- **SlotEnrichmentStarted**: ids, `total_slots`, `enricher_count`.
- **SlotEnrichmentCompleted**: ids, `total_slots`, `slots_changed`,
  `invocations`, `errors_count`.
- **SlotConfidenceUpdated**: ids, `slot_id`, `old_confidence`,
  `new_confidence`, `source` (`LlmPrimary`, `UserReply`, `DeterministicCheck`,
  …).
- **SlotGraphDiff**: `execution_id`, `source`, `inserted`, `updated`,
  `removed`, `total_slots`.
- **ClarifiedTaskReady**: ids, `clarified_task`, `objectives_count`,
  `constraints_count`, `confidence`.
- **ParameterInferenceAttempted / ParameterInferred /
  ParameterInferenceFailed**: `execution_id`, `parameter_name`; Attempted +
  `priority`; Inferred + `inferred_value`, `confidence`, `method` (`LLMBased` /
  `RuleBased` / `Historical` / `Default` / `AutoFill`); Failed + `confidence`,
  `reason`.
- **ParameterDiscoveryAttempted / ParameterDiscovered /
  ParameterDiscoveryFailed**: `execution_id`, `parameter_name`;
  Attempted/Discovered + `discovery_method`; Discovered + `discovered_value`,
  `confidence`, `external_actions_performed`; Failed + `reason`.
- **ParameterResolutionProgress**: `execution_id`, `total_parameters`,
  `resolved_count`, `inferred_count`, `discovered_count`, `deferred_count`,
  `remaining_count`.

## Clarification

Session telemetry only; the human-response payload is `HitlRequested` /
`HitlResolved` with `source: "clarification"`.

- **ClarificationSessionSnapshot**: `execution_id`, `state`,
  `total_questions`, `waiting_on_user`, `queued`, `answered`, `cancelled`,
  `pending_question_ids: string[]`, `active_batch?`, `last_question_asked_at?`.
- **ClarificationConfidenceSnapshot**: `execution_id`, `question_id`,
  `trigger`, `overall_confidence`, `unresolved_count`, `slot_deltas`,
  `question_created_at`, `answered_at`.
- **ClarificationMetricsSnapshot** (process-wide, no execution_id):
  `total_sessions_started`, `total_sessions_completed`, `active_sessions`,
  `avg_session_duration_ms?`, `avg_questions_per_session?`,
  `guardrail_timeouts`, `guardrail_question_caps`, `guardrail_round_caps`.

## Execution lifecycle

All carry `execution_id`; the first ten also `plan_id`.

- **ExecutionStarted**: `steps_total`.
- **ExecutionStepStarted**: `step_index`, `step_id`, `steps_total`.
- **ExecutionStepCompleted**: `step_index`, `step_id`, `success`.
- **ExecutionPaused**: `step_index`, `step_id`, `reason`; also the reconnect
  re-emit for a manual pause.
- **ExecutionResumed**: `step_index`, `step_id?`, `mode`.
- **ExecutionFailed**: `step_index`, `step_id`, `error`.
- **ExecutionCancelled**: `step_index`.
- **ExecutionCompleted**: `steps_total`, `success`.
- **ExecutionInflightResent**: `step_index`, `step_id`, `request_id`,
  `attempt_count`.
- **ExecutionInflightDropped**: the same without `attempt_count`.
- **ExecutionRestoreFailed**: `reason`, `note?`.
- **ExecutionStatusChanged**: `task_id?`, `root_execution_id?`,
  `previous_status`, `new_status`, `reason?`.
- **ExecutionResponsibilityChanged**: `parent_execution_id?`, `task_id?`,
  `root_execution_id?`, `waiting_state`, `active_owner_agent_id`,
  `owner_stack: string[]`, `active_delegation_group: string[]`.
- **WorkflowResumed**: `question_id?`, `resume_mode`, `answered_count`,
  `pending_count`.
- **WorkflowStageResumed**: `stage_name`, `stage_context`, `attempt`,
  `reused_checkpoint`, `checkpoint_hash?`, `reused_stages: string[]`.
- **WorkflowResumeFailed**: `question_id?`, `error`.

When a failed `agentic-continue` restores a pause, the re-emitted
`AgenticWaiting*` carries the effective requested `execution_id`; treat it as
canonical.

## LLM analysis

- **LLMAnalysisStarted / LLMAnalysisCompleted / LLMAnalysisFailed**
  (planner-side): `execution_id`, `correlation_id`, `provider`, `stage`;
  Started + `query_length`; Completed + `response_length`, `duration_ms`;
  Failed + `error_type` (`api_key_missing` / `timeout` / `invalid_response` /
  `network_error`), `error_message`.
- **LLMRequestSent** (agentic loop): `execution_id`, `plan_id`, `step_id?`,
  `step_index?`, `capability` (`page_understanding`, `decision`, …),
  `request_summary`, `input_tokens_estimate?`, `budget_remaining`.
- **InferenceAttempted** (agentic parameter inference): `execution_id`,
  `plan_id`, `step_id?`, `parameter`, `inferred_value?`, `confidence`,
  `reason`, `accepted`.
- **ThinkingModeActivated / ThinkingModeCompleted** (adaptive-profile
  escalation): `execution_id`, `chat_session_id?`, `chat_turn_id`,
  `adaptive_profile`; Activated + `fast_profile`, `thinking_profile`,
  `reason?`. Completed fires at turn end so the UI clears the chip.

- **LLMResponseReceived** (agentic-loop response):
  - `correlation: LlmEventCorrelation?`: canonical recorder input;
    `correlation.activity_id` joins the call to its span (and `ActivityCost`);
    absent outside an instrumented span. `llm_trace_activation` rebuilds
    `LlmTraceContext` from it alone.
  - `capability` (e.g. `decision`), `success`, `decision_summary`,
    `latency_ms`, `error?`, `attempt` (u32, 1-based), `ttft_ms?`
    (time-to-first-token), `started_at_ms` (i64; 0 when unmeasured).
  - `cost` (f64 USD via `magicllm::pricing`; `0.0` when no pricing row
    matches); `provider`, lowercase (`anthropic`, `minimax`, `openai`,
    `openrouter`, `gemini`, `ollama`, `yutori`); `model` (effective id);
    `profile?`; `operation` (e.g. `agentic_decision`); `response_kind`
    (`tool_call` / `text` / `streaming` / `reasoning_only` / `error`).
  - u32 tokens: `input_tokens` / `output_tokens` / `reasoning_tokens`,
    `cache_read_tokens` / `cache_creation_tokens`, `search_calls`
    (`server_web_search` lane); `audio_input_tokens` / `audio_output_tokens` /
    `audio_cached_tokens` (optional, realtime voice only). Zero buckets mean
    something only when `usage_reported` is true.
  - `task_id` / `agent_id` / `delegated_agent_id` / `chat_session_id`
    (optional); `reasoning_summary?` (provider chain-of-thought summary, when
    requested).

## Agentic execution

Observe-decide-execute loop. Pause **payloads** are on `HitlRequested`; the
`AgenticWaiting*` variants are slim lifecycle markers (Hitl category).

- **AgenticExecutionStarted**: `execution_id`, `plan_id`, `step_id`, `goal`,
  `success_criteria`, `max_iterations`, `hint_action?`, `agent_id?`.
- **AgenticIterationStarted**: `execution_id`, `plan_id`, `step_id`,
  `iteration` (1-indexed), `environment_type` (`browser` / `filesystem` /
  `http` / `shell`).
- **AgenticIterationCompleted**: end of every iteration. `duration_ms`,
  `outcome`: `action_executed` / `decision_rejected` / `loop_continue` /
  `terminal` / `paused` / `cancelled`.
- **AgenticStepStarted / AgenticStepCompleted / AgenticStepFailed /
  AgenticStepSkipped** (taskplan steps): `execution_id`, `plan_id`, `step_id`,
  `iteration`; Skipped + `reason?`. Started precedes the action, for the step in
  `decision_metadata.step_started` or one the action finishes while not current
  (`LoopProtectiveState.current_plan_step`), so every finished step has a
  Started; Skipped (`step_skipped`) precedes the replacing action. Canonical
  `step.started` / `step.completed` / `step.failed` / `step.skipped` drive
  off-plan attribution (`docs/archive/plans/2026-10-01-plan-step-progress.md`).
- **AgenticStepStuckWarning** (observability only): `iteration`,
  `is_preflight` (true = capability PREFLIGHT blocker; not a stuck signal),
  `consecutive_count`, `recent_actions` (last ≤ 3 `action_summary` strings).
- **AgenticPageUnderstanding**: `observation_id`, `iteration`, `page_stage`,
  `element_count`, `appears_loading`, `url?`, `confidence`, `has_screenshot`.
- **AgenticDecisionMade**: `iteration`, `decision_type` (`execute`,
  `goal_reached`, `cannot_proceed`, `need_user_input`, `spawn_sub_goal`,
  `delegate_to_agent`, `delegate_to_agent_async`, `create_task`),
  `action_summary?`, `reasoning`, `confidence`, `thinking?`, `evidence?`,
  `tool_name?`, `action_type?`, `element_id?`, `candidates_count?`,
  `raw_decision?`.
- **AgenticActionStarted**: `iteration`, `action_type`, `target`, `call_id`,
  `detail?`. Emitted inline (not journalled, which waits for Apply commit) at
  dispatch, so a long tool call shows while running; `call_id` repeats on
  `AgenticActionExecuted`. `detail` is a human summary; for a governed MCP
  `<pack>__call_tool`: `<pack> · <remote tool> — <one allowlisted argument>`
  (`sql`, `query`, `prompt`, `term_queries`, `semantic_queries`, `id`, `uris`),
  never the argument payload.
- **AgenticActionExecuted**: `iteration`, `action_type`, `target`, `success`,
  `latency_ms`, `error?`, `call_id?`, `detail?`, for every executed action. A
  failure (provider error or unsuccessful result) has `success: false`, a
  bounded `error` and the success-identical `action_type`/`target`. Canonical:
  `tool.succeeded` / `tool.failed`.
- **AgenticClickFallbackUsed** (legacy; browser automation now returns
  failures to the loop): `original_selector`, `original_error`,
  `coordinates: (f64, f64)`, `fallback_success`, `fallback_error?`,
  `latency_ms`.
- **AgenticExecutionCompleted**: `outcome`, `iterations_used`,
  `artifacts: string[]`, `duration_ms`, `summary`, `refinement_pass_index`
  (0 = original), `refinement_pending`, `yield_payload?` (structured Yield
  termination). Outcomes: `success`, `failed`, `max_iterations_reached`,
  `loop_detected` (+ `loop_detection_type`, `loop_repeated_action`,
  `loop_recommendation`, `loop_cycle_pattern`, `loop_similarity`),
  `waiting_for_user`, `waiting_for_confirmation`, `budget_exhausted`
  (+ `budget_dimension`, `budget_details`), `cannot_proceed`
  (+ `cannot_proceed_reason`).
- **AgenticMaxIterationsReached** (marker): `execution_id`, `plan_id`,
  `step_id`, `iterations_used`, `pause_state_id?`, `agent_id?`, `goal_id?`,
  `cycle_id?`. Prompt: `HitlRequested { source: "agentic",
  input_schema.pause_kind: "max_iterations" }`.
- **AgenticWaitingForUser** (marker): `execution_id`, `plan_id`, `step_id`,
  `iteration` (usize), `pause_state_id?` (resume routing id; reconnect dedupe
  key), `correlation_id?` (only for `diff_approval` pauses, `ccp-<uuid>`),
  `is_retry?` (re-ask after rejection), `retry_count?`,
  `agent_id` / `goal_id` / `cycle_id` (optional routing),
  `escalation_trigger?` (`cannot_proceed` / `loop_detected` when
  `on_failure: ask_user`; `None` for ordinary input). `question`,
  `input_type`, `hint`, `options`, `previous_answer`, `retry_reason` ride
  `HitlRequested { source: "agentic" }` from
  `emit_hitl_requested_for_agentic_pause`, published only once the pause is
  answerable (after commit or `publish_terminal_loop_pause`).
- **AgenticWaitingForConfirmation** (marker): `execution_id`, `plan_id`,
  `step_id`, `iteration`, `pause_state_id?`, `agent_id` / `goal_id` /
  `cycle_id`. `action_summary` / `reason` / `action_type` ride
  `HitlRequested { source: "agentic" }` from
  `emit_hitl_requested_for_agentic_confirmation`.
- **AgenticResumed**: `pause_state_id?`, `plan_id`, `step_id`,
  `resumed_from_iteration`, `input_type`, `user_responded`, agent routing ids.
- **DomChangeDetected** (magicutor SSE backchannel): `correlation_id`,
  `total_changes`, `nodes_added`, `nodes_removed`, `signals: string[]`,
  `initial_url?`, `final_url?`, `action_type?`, `selector?`, `outcome`
  (`success` / `failed`).
- **SubGoalRequested / SubGoalOutcome**: `execution_id`, `plan_id`,
  `parent_step_id`, `sub_goal`; Requested + `budget_iterations`, `depth`;
  Outcome + `outcome`, `iterations_used`, `duration_ms`.

## Human-in-the-Loop (Canonical)

Every HITL surface emits `HitlRequested` when a prompt is pending and
`HitlResolved` when it is answered, expires, is cancelled or dismissed; agentic
pauses **also** emit the `AgenticWaiting*` / `AgenticMaxIterationsReached`
markers, approvals an `agent.update` `approval_requested`. Consumers dispatch
on `source`; `pendingHitlStore` is authoritative for what waits on a human.

State survives restart in the private `pending_hitl_lifecycle.v3.sqlite3`
store (the legacy `pending_hitl_lifecycle.jsonl` is imported once, committed
before readiness; a durable directory fence stops an older binary reopening
split authority). Lifecycle records, `input_schema` values and unscoped legacy
HITL get structural and byte ceilings before serialization or fan-out. See
[`hitl-attention.md`](./hitl-attention.md#restart-durability).

Privacy exception: the host-sealed app-owner-notification family is consumed
at the broadcaster boundary, before the shared ring; owner surfaces read the
scoped `UserRequestService` record, and generic subscribers and
`pendingHitlStore` never see its prompt body. Pending rows are content-free
(`body = NULL`, only expiry and an opaque generation binding the live
UserRequest, no body digest); a
resolved row may keep the fingerprint of its content-free `HitlResolved`
envelope, which cannot reconstruct or authorize the body.

Respond: `POST /api/magician/v2/hitl/{correlation_id}/respond`; `source`
selects the arm:

- `agentic`, `primitive`, `inner_loop`, `escalation` → agentic pause resume
  (`is_agentic_runtime_hitl_source`)
- `user_request` → `UserRequestService::respond_scoped`
- `approval` → `resolve_approval`
- `clarification` → V3 task-plan clarification, then ask-loop
- `plan_approval` → approve/reject the plan version
- `diff_approval` → apply/reject a staged file-edit proposal
- `bot_auth` → bot auth HITL broker; `mcp_oauth` → MCP OAuth API
- `service_health` → dismiss only (`choice` `dismiss`, else `400`
  `service_health_requires_dismiss`; not pending → `409`)

`progress_channel_seam/normalize.rs` projects HITL onto chat/feed
`AgentNotification` rows; `feed/materializer.rs` buckets `FeedItemType` by
`metadata.source`; per-row `attention_kind` labels stay surface-side. The
envelope mirrors `HitlRequest` in
`ui/unified-ui/src/lib/hitl/types.ts`.

### HitlRequested

- `correlation_id`: stable for the request lifetime (`pause_state_id` /
  `request_id` / `approval_id` / `question_id`).
- `source`: `agentic`, `primitive`, `inner_loop`, `user_request`, `approval`,
  `plan_approval`, `clarification`, `escalation`, `diff_approval`,
  `service_health`, `bot_auth`, `mcp_oauth`.
- `input_type`: `text`, `password`, `otp`, `choice`, `multi_choice`,
  `confirmation`, `external_action`, `file_path`, `guidance`,
  `tool_authorization`, `sandbox_override`, `diff_approval`, `form`.
- `prompt` (question, summary or escalation banner); `hint?`.
- `input_schema?`: `placeholder`, `options`, `multiline`, `allow_other`,
  `min_selections`, `max_selections`; `sensitive`, the value-free
  `SensitiveInputSpec` (`kind`, `fields[]`, `provenance`, `one_time`,
  `collection_deadline_ms`) on secret asks from `user_request` and `agentic`,
  which clients mask by
  ([Sensitivity contract](hitl-attention.md#sensitivity-contract-secure-hitl-credentials-p1));
  clarification `stage` / `blocker_type` / `urgency` / `source_slot_id` /
  `chain_id` / `chain_position` / `chain_total`; approval `approval_id` /
  `goal_id` / `cycle_id` / `trigger_seq` / `pending_action_count` /
  `expires_at`.
- `task_id` / `execution_id` / `agent_id` (when available); `principal` /
  `workspace`, required for visibility filtering (`system` for purely system
  prompts).

### HitlResolved

`correlation_id` and `source` (same as the request), `outcome` (`responded`,
`expired`, `cancelled`, `dismissed`), `decision?` (opaque per source:
`approve` / `reject`, `confirm` / `deny`, …), and `task_id` / `execution_id` /
`agent_id` / `principal` / `workspace` scoped like the request.

### Critical-request delivery and code retrieval

Value-free; optional `principal` / `workspace` (missing → `system`/`system`
only). See [`critical-request-delivery.md`](./critical-request-delivery.md),
[`verification-code-retrieval.md`](./verification-code-retrieval.md).

- **CriticalRequestAlert**: `delivery_id`, `correlation_id`, `channel_type`,
  `kind` (`request` / `test`), `alert` (safe card: `service_alias`, `reason`,
  `text`, `deadline_ms?`, `open_url?`; never a prompt, option, answer or
  token), `revision`, `deadline_ms?`. The channel's bot claims it via
  `POST /api/magician/v2/hitl/deliveries/{id}/claim` to learn the address.
- **CriticalRequestRetired**: `delivery_id`, `correlation_id`, `channel_type`,
  `outcome` (`responded` / `expired` / `cancelled` / `dismissed` /
  `superseded`).
- **VerificationRetrievalStatus** (pending `otp` ask): `correlation_id`,
  `status` (`waiting` / `code_used` / `ambiguous` / `unavailable` /
  `stopped`), `sources: string[]`, `reason?`; never the code or message.

## Agent lifecycle

- **AgentCycleStarted**: `agent_id`, `goal_id`, `cycle_id`, `execution_id?`,
  `goal`.
- **AgentCycleCompleted**: same ids, `outcome` (`success` / `failure` /
  `paused` / `cancelled`), `iterations_used`.
- **AgentTriggered**: `agent_id`, `goal_id`, `trigger` (`manual` / `schedule` /
  `webhook` / `event`).
- **AgentEvent**: `event: AgentEventEnvelope` (`event_type`, `agent_id`,
  optional `principal` / `workspace`, `payload`, `timestamp`) for `reasoning.*`,
  `tool.call.*`, `plan.*`, `agent.update` and other non-variant GAUI rows
  ([streaming](#reasoning--tool-call-streaming),
  [operator feed](#operator-feed-agent-update)).
- **AgentDefinitionChanged**: required `principal`, `workspace`, `agent_id`;
  session-scoped chat lifecycle subscriptions re-materialize.

## Task CRUD

Required `principal`, `workspace`, `task_id`; optional `execution_id`,
`ui_thread_id`; `title`. Created + `created_at` / `updated_at`; Updated +
`updated_at` (`persist_execution_outcome` also emits `TaskUpdated` when
`outcome.is_terminal`); Deleted + `deleted_at`, from
`archive_task_with_options` (archive and delete) once the task leaves the
list. Phones refresh on any `Task*` event, the web task store on Deleted.

## Feed

- **FeedItemCreated**: `item: FeedItem` plus top-level `timestamp`
  (`extractTimestamp` reads that, not `item.created_at`).
- **FeedItemUpdated**: `principal`, `workspace`, `id`, `task_id?`,
  `ui_thread_id?`, `execution_id?`, `patch: FeedItemPatch`, `timestamp`.
- **FeedItemRemoved**: the same without `patch`.
- **ExecutionPanelDelta**: `principal`, `workspace`, `task_id?`,
  `execution_id?`, `state: ExecutionPanelState` (full curated snapshot, not a
  patch).

## Chat sessions

### ChatMessageReceived
`session_id`, `message: ChatMessage` (chat REST shape), `origin_channel?`, for
the web chat UI and consumer-channel bots; `message.presentation` is stripped
unless `supports_structured_presentation=true`. A `TaskStatusUpdate` message for a task a chat turn started (delegate cards,
planning cards for a registered turn, progress-channel cards whose subscription
names a turn) carries that turn's real `message.chat_turn_id`, never the
synthetic `chat-task-<task>-<session>`; `null` means no turn (plan mode,
`subscribe_to_task`, scheduler).

### Chat-inline `llm.requested` (AgentEvent)
Before each chat tool-loop model call: `chat_turn_id`, `model`, `engine`,
`harness_model`, `capability: "chat.inline"`, `iteration`, prompt-size
counters, `started_at`. `engine` is `magician` (native) or a harness
(`claude_code`, `codex`, `codex_app_server`, `grok`, `agy`, `pi`). `model` is
the native profile name, or `"<Engine> · <harness_model>"` (e.g.
`"Claude Code · opus"`), or the engine name (Claude Code, Codex, Grok,
Antigravity, Coding agent (`pi`)) on the CLI default. `harness_model`: the
CLI's model; `null` for native or the CLI default (a `pi` turn without one
names its profile). A declining harness (`Ok(None)`) is followed by a native
iteration with a fresh `llm.requested`, so the label always names the mouth
that answered; `llm.succeeded` / `LLMResponseReceived` carry the exact model.

### Chat delegate terminal `chat.delegate.status_changed` (AgentEvent)
Emitted by the delegate teardown watcher's terminal catch-up before it posts
the terminal `TaskStatusUpdate` under the same `chat_turn_id`, only for a real
turn id and only if the chat has no terminal card for the task yet. Payload
(as the live progress-channel row): `event_id`, `chat_turn_id`, `task_id`,
`execution_id`, `status` (`completed`/`failed`/`cancelled`), `summary`,
`output_files`, `synthesis_pending: false`, `completion_kind`,
`target_agent_id`, `chat_inline_delegate_agent_id`,
`recovery_origin: "delegate_terminal_watcher"`.

## Observability

- **ObservabilityAlert**: `execution_id`, `alert_type`, `details`.
- **Heartbeat**: `timestamp` only; visible to every WS scope. Connection-ready
  signal and post-lag keepalive on `/realtime/ws` (no `skipped` field).
- **ProgressEvent**: `message: ProgressMessage` (progress_channel_seam);
  producers emit only onto this bus and the progress router fans out. Unrelated
  to `ActivityProgress`.
- **ThinkingMapUpdated**: required scope; change notice only (`map_id`,
  `revision`); clients re-fetch `GET /thinking-maps/{id}`.
- **ThinkingMapInterpretProgress** (best-effort): `map_id`, `utterance_id`,
  `stage` (`preparing` / `loading_context` / `facilitating` / `parsing` /
  `shaping` / `idle`; tolerate unknown), `detail?`, `node_count?`. The result
  still arrives as `ThinkingMapUpdated` plus the HTTP response.
- **ShellOutputChunk**: batched every 100ms from `execute_bash_action`.
  `execution_id`, `step_id`, `step_index`, `command` (first chunk only),
  `stream` (`stdout` / `stderr`), `data`, `sequence`, `is_final`,
  `exit_code?` (final only).
- **InteractivePtyChunk**: live PTY bytes for the xterm pane. Required
  `principal` / `workspace`; `session_id`, `ui_thread_id?`, `program?`,
  `offset_start`, `offset_end`, `bytes_b64` (decode before `term.write()`),
  **`timestamp_ms`** (not `timestamp`).

## Reasoning & Tool-Call Streaming

GAUI strings in `AgentEvent` envelopes (via
`RuntimeTransportBroadcaster::emit_named`), not enum variants;
`GAUI_EVENT_TAXONOMY` maps `reasoning.*` → Llm, `tool.call.*` → Tool. All
carry `execution_id`, the key (`payload.execution_id`) the chat fan-out
registry re-stamps delegated events by.

- **reasoning.start**: `trace_id`, `execution_id` (null from the chat outer
  loop), `model?`, `budget_tokens?`, `started_at`.
- **reasoning.content**: `trace_id`, `execution_id`, `delta`. Governed app
  turns omit content after their monotonic persistence fence but keep
  start/end metadata.
- **reasoning.end**: `trace_id`, `execution_id`, `total_tokens?`,
  `duration_ms?`, `ended_at`.
- **tool.call.started**: `call_id`, `tool_name`, `execution_id`, `step_id?`,
  `args?` (governed app calls expose only an omission marker, canonical digest
  and bounded shape), `started_at`.
- **tool.call.args**: `call_id`, `execution_id`, `delta`.
- **tool.call.finished**: `call_id`, `tool_name`, `execution_id`, `success`,
  `duration_ms?`, `content_preview?` (≤240 chars), `exit_code?`, `error?`,
  `finished_at`.

### Chat fan-out for delegated executions

For an `execution_id` with a registered `chat_fanout` target (via
`delegate_to_agent`), each event above is copied under the chat agent's
`agent_id+scope` with `chat_delivery_kind: "inline_delegate"`,
`chat_session_id`, `origin_agent_id` (the delegate), `chat_turn_id` (if
supplied) and, on `tool.call.*`, `call_id = delegate-{execution_id}/{orig_call_id}`
plus `origin_call_id` (the unrewritten id). The delegate's original is still emitted; no copy when
the chat agent delegates to itself.

## Activity

Emitted by the tracing layer (`runtime_activity_layer.rs`,
`EventCategory::Activity`): each instrumented span becomes a started/finished
pair, each INFO-or-above log line a progress row. UI:
[`runtime-activity-view.md`](../unified-ui/runtime-activity-view.md).

- Unscoped spans fall back to **`anonymous` / `default`**, not `system` /
  `system` (cross-scope passes such as `attention_rank_recompute_pass` declare
  `system` / `system`); an event still missing `principal`/`workspace` is
  fail-closed to `system`/`system` by `event_visible_to_scope`.
- `kind` ∈ `agent` / `background` / `capability` / `llm` / `process` /
  `runtime`; `outcome` ∈ `success` / `error` / `cancelled` / `closed`.
  `message`, the only free-form field, is capped at 2 KiB and must never carry
  credentials, prompt bodies, tool arguments or user content.
- The queue (4096) evicts the oldest when full; `dropped` (every variant) is
  the process-cumulative eviction count and the only source of
  incompleteness, since `init_tracing` pins the filter to the process
  baseline, not `--log-level`.
- `seq` is a decimal string (counter from a 63-bit random base); dedupe on it,
  since JSON numbers past 2^53 lose precision in browsers.
- `workload_class`, `agent_id`, `thread_id`, `task_id`, `model` inherit from
  the nearest tracked ancestor unless declared, else are omitted (**no
  default**); `operation` does not inherit.

Variants:

- **ActivityStarted**: `activity_id`, `parent_activity_id?`, `name`, `target`,
  `kind`, `principal?`, `workspace?`, `workload_class?` (`foreground_chat` /
  `interactive_task` / `ambient` / `scheduled` / `autonomous_task` / `memory` /
  `system` / `evaluation` / `comms_assist`, byte-for-byte
  `LlmWorkloadClass::as_str()`), `agent_id?`, `thread_id?`, `task_id?`,
  `model?`, `operation?` (`LLMOperation` name, only when declared), `dropped`,
  `seq`, `timestamp`.
- **ActivityFinished**: `activity_id`, `duration_ms`, `outcome` (default
  `closed`), scope, `dropped`, `seq`.
- **ActivityProgress**: `activity_id?` (absent below the span floor; renders
  at the root), `level`, `message`, `target`, scope, `dropped`, `seq`.
- **ActivityCost**: from the canonical LLM recorder **after** the span
  closed; attach to a span the consumer already holds, else drop. No
  `activity_id`, unpriced call or no model call → no event (not zero).
  Accumulate per commodity, never across. Fields: `activity_id`; `cost_microunits` (u64, millionths of
  a `commodity` unit); `commodity` (required: `usd` vendor-priced, `local`
  operator hardware, or package units such as `tavily_credit`);
  `input_tokens` / `output_tokens` (u64?); `dropped` (always `0`;
  receiver-side loss shows as `__events_lagged__`); `seq` (the logical
  `llm_call_id`).
- **DecisionShadowAgreement** / **DecisionAccountingGap** (Gap is Warn):
  versioned, content-free memory-classification evidence joined to the
  Decision Model ledger by call id. Required `principal` / `workspace`, `timestamp`;
  Agreement + `record`; Gap + `operation`, `request_id`, `reason`.

## Operator Feed (Agent Update)

Typed events for the `/feed` operator timeline
([agent-updates.md](./agent-updates.md)). `publish_agent_update` wraps an
`AgentUpdate` in `AgentEventEnvelope` (`event_type = "agent.update"`,
`agent_id`, `principal` / `workspace` required for journal routing,
`timestamp`) as `RuntimeTransportEvent::AgentEvent`; on `/realtime/ws` read
`data.event.payload.kind`, while `/events?event_type=agent.update` matches
(it unwraps). Common payload: `id` (ULID), `ts`, `workspace_id`, `agent_id`,
`thread_id?`, `cycle_id?`, `kind`.

**Variants (30)**, `AgentUpdateKind` in
`magician/src/magician_v2/agents/agent_update.rs` (`kind`: fields):

- Agent: `agent_created` (`name`, `agent_kind`), `agent_updated`
  (`changed_fields: string[]`), `agent_deleted`, `agent_paused` (`reason?`),
  `agent_resumed`.
- Cycle: `cycle_started` (`focus_area?`, `trigger`), `cycle_completed`
  (`outcome`: `succeeded`/`failed`/`paused`/`partially_succeeded`;
  `duration_ms`), `cycle_failed` (`error`, `duration_ms`), `cycle_paused`
  (`reason`).
- Goal: `goal_completed` (`goal_id`, `artifacts[]`), `goal_failed`
  (`goal_id`, `error`), `goal_recovered` (success after prior failures;
  `goal_id`, `recovery_strategy`).
- Approval: `approval_requested` (`approval_id`, `tool?`, `action?`,
  `params?`, `pending_action_count?`), `approval_resolved` (`approval_id`,
  `decision` `approve`/`reject`, `resolver`), `approval_expired`
  (`approval_id`).
- Artifact: `artifact_created` (`artifact`:
  `{artifact_id, kind, surface_url?, label?}`), `artifact_create_failed`
  (`attempted_kind`, `error`).
- Circuit breaker: `circuit_opened` (`scope`, `reason`, `next_retry_at?`),
  `circuit_recovered` (`scope`, `recovered_at`; only when
  `previous_failures >= max_consecutive_failures`).
- Task: `task_created` (entering `planning` or `ready`; `task_id`, `title`),
  `task_updated` (`task_id`, `changed_fields[]`), `task_completed`
  (`task_id`, `artifacts[]`), `task_failed` (`failed` or `cancelled`;
  `task_id`, `error`).
- Learning: `memory_report` (`summary`, `episodes_processed`,
  `corrections_applied`), `feedback_generated` (`corrections`,
  `success_patterns`), `tier_consolidated` (`source`, `target`,
  `records_written`, `rule`).
- Delegation: `delegation_issued` (`from_agent`, `to_agent`, `task_id`),
  `delegation_resolved` (same plus `outcome`).
- `feed_stalled` (no events for a cycle past `stall_after`; `silent_for_ms`),
  `published_surface_changed` (`surface_id`, `url`).

`AgentUpdateJournal` appends each envelope to
`scopes/<principal>/<workspace>/updates.jsonl` (rotates at 50 MB; unscoped
events dropped), served newest-first by
`GET /api/magician/v2/updates?since=&limit=&agent=&kind=&thread=`, scoped by
the workspace-bound bearer (`limit` default 200, max 1000, after filters).

## Event flow

Agentic loop: `AgenticExecutionStarted`, then per iteration
`AgenticIterationStarted`, `LLMRequestSent` / `LLMResponseReceived`,
`AgenticPageUnderstanding` (browser), `AgenticDecisionMade`,
`AgenticActionStarted`, `AgenticActionExecuted`, `AgenticIterationCompleted`.
A human pause emits the `AgenticWaitingForUser` /
`AgenticWaitingForConfirmation` marker plus
`HitlRequested { source: "agentic" }`, then `HitlResolved` /
`AgenticResumed`. Ends with `AgenticExecutionCompleted`. A message runs
`MessageProcessingStarted` → pipeline/plan events → `ExecutionStarted` …
`ExecutionCompleted` → `MessageCompleted`.

## Related

- [`v2-api-guide.md`](./v2-api-guide.md#unified-event-stream--get-apimagicianv3events) — `/events` query contract
- [`agent-updates.md`](./agent-updates.md) — operator-feed architecture
- [`../unified-ui/runtime-activity-view.md`](../unified-ui/runtime-activity-view.md) — activity UI
- Historical specs: `../../archive/magician/AGENTIC_EXECUTION_OBSERVABILITY.md`, `../../archive/magician/EXECUTION_OBSERVABILITY.md`
