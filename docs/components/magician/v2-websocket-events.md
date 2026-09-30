# Magician V2 WebSocket Events Reference

Current-state contract for `RuntimeTransportEvent` — the tagged enum in
`magician/src/magician_v2/realtime_events.rs` (107 variants). Two HTTP
surfaces deliver the same enum; a third on-disk surface stores a mapped
GAUI form.

- **WebSocket**: `GET /api/magician/v2/realtime/ws` — handler
  `magician-api/src/websocket_handler.rs`. Default local URL
  `ws://127.0.0.1:3002/api/magician/v2/realtime/ws`.
- **HTTP live tail**: `GET /api/magician/v3/events` — NDJSON over chunked
  HTTP; see [`v2-api-guide.md`](./v2-api-guide.md#unified-event-stream--get-apimagicianv3events).
- **Wire**: `#[serde(tag = "event_type", content = "data")]`. Outer
  `event_type` is the PascalCase variant name. Payload fields live under
  `data`.
- **Taxonomy**: `magician-event-taxonomy` (`EVENT_TAXONOMY_TABLE` +
  `GAUI_EVENT_TAXONOMY`). `make event-taxonomy-codegen` regenerates the
  TypeScript mirror; `make event-taxonomy-check` fails the build on drift.

`UserRequestPending` / `UserRequestResolved`, `ClarificationQueued` /
`ClarificationResponseReceived`, and `V3PlanningClarificationNeeded` /
`V3PlanningClarificationResolved` are not on the enum. Those surfaces emit
`HitlRequested` / `HitlResolved` only.

---

## Transports

### WebSocket — `GET /api/magician/v2/realtime/ws`

Bearer-scoped. Browser clients offer `magician-events-v2` followed by the
auth-only `magician-bearer.<token>` protocol. The handler selects
`magician-events-v2`; it never echoes the credential. Native clients may keep
using an `Authorization` header and need not offer a subprotocol. Query params
(`WsConnectParams`):

| Param | Role |
|-------|------|
| `execution_id` | On connect, re-emit pending pauses for that execution |
| `agent_id` | Also re-emit `FullPauseStore::get_pending_for_agent()` |
| `supports_structured_presentation` | When false/omitted, `ChatMessageReceived.message.presentation` is stripped |

Origin check (`validate_origin`): missing Origin is allowed (non-browser
clients); a present Origin must match `localhost` / `127.0.0.1` / `[::1]`
or the request Host hostname, else `403 origin_mismatch`. Invalid
`execution_id` / `agent_id` query values return `400`. A reconnect that
arrives while legacy pause-index repair is still running returns `503`
`legacy_pause_index_repair_pending`.

The session:

- Sends an immediate `Heartbeat` as the connection-ready signal (same
  shape as later heartbeats — there is no separate Connected event).
- Re-emits pending pauses as slim lifecycle markers
  (`AgenticWaitingForUser`, `AgenticWaitingForConfirmation`,
  `AgenticMaxIterationsReached`, or `ExecutionPaused` for a manual pause).
  Clients must dedupe on `pause_state_id`. HITL prompt fields are **not**
  on those re-emits; subscribe to `HitlRequested`.
- Forwards every `RuntimeTransportEvent` that
  `event_visible_to_scope` accepts for the bearer principal/workspace.
- Does **not** apply taxonomy (`category` / `severity` / `user_relevant`)
  filters — those exist only on `/events`.
- Server ping every 5s; client timeout 30s; inbound frames capped at 64 KiB.

If the broadcast receiver lags, the handler logs a warning and sends a
plain `Heartbeat`. That frame does **not** carry `skipped`. Contrast
`GET /v3/events`, which emits an `__events_lagged__` sentinel.

Inbound client JSON (`type` discriminator):

| `type` | Effect |
|--------|--------|
| `agent.ui.snapshot_request` | `{ agent_id }` → `agent.ui.snapshot` or `agent.ui.snapshot_error` |
| `ui.interaction` | `{ agent_id, component_id, goal_id?, trigger?, request_id? }` → `ui.interaction.ack` / `ui.interaction.error`. Rate-limited to 1 trigger / 5s per `(agent_id, component_id)` |

Unknown inbound `type` values are dropped after a protocol `error` frame
when `type` is missing.

### HTTP — `GET /api/magician/v3/events`

Same enum, NDJSON (`application/x-ndjson`). Query (`EventsQuery`):
`workspace`, `execution_id`, `task_id`, `agent_id`, `since`, `before`,
`limit` (default `EVENTS_RETENTION_MIN_COUNT` = 2000), `category`,
`severity`, `user_relevant`, `event_type` (substring), `search`,
`backfill_only`.

Taxonomy filters match `RuntimeTransportEvent::taxonomy()` except for
`AgentEvent`: `/events` unwraps `data.event.event_type` and looks that
string up in `GAUI_EVENT_TAXONOMY` so `tool.call.*` / `reasoning.*` /
`agent.update` filter as their inner type, not as catch-all `Agent`.

Backfill:

- Both `execution_id` and `task_id` → per-execution canonical
  `events.jsonl` (GAUI dotted names via `ArtifactV2EventType`).
- Otherwise → per-scope transport log (RuntimeTransportEvent JSON) plus
  recent per-execution files. Cross-scope scan is capped at 50_000 rows
  (`__events_partial__` if the cap trips). Default window 24h when `since`
  is omitted.

Live lag on this endpoint emits:

```json
{"event_type":"__events_lagged__","timestamp_ms":…,"skipped":N,"message":"…"}
```

`POST /api/magician/v3/events/debug-emit` injects a synthetic
`RuntimeTransportEvent` via `emit_transport_only` (live only, not
canonical jsonl). Always enabled.

---

## Wire format and AgentEvent unwrap

Typed variants serialize as:

```json
{"event_type":"HitlRequested","data":{"correlation_id":"…","source":"agentic",…}}
```

`AgentEvent` is the extensible envelope. Inner `AgentEventEnvelope`
carries the GAUI / operator string (`reasoning.start`, `tool.call.finished`,
`agent.update`, …):

```json
{"event_type":"AgentEvent","data":{"event":{"event_type":"reasoning.start","agent_id":"…","payload":{…},"timestamp":…}}}
```

`/events` filter helpers (`unwrap_event_type`) treat the inner string as
the event type when the outer tag is `AgentEvent`. The WebSocket does not
unwrap — clients matching `event_type === "reasoning.start"` on the WS
must look at `data.event.event_type`.

---

## Taxonomy

`magician-event-taxonomy` is the source of operator metadata. Two tables
must stay in lockstep with the enum:

| Table | Key | Used for |
|-------|-----|----------|
| `EVENT_TAXONOMY_TABLE` | PascalCase variant | `RuntimeTransportEvent::taxonomy()` — category, severity, `user_relevant` |
| `GAUI_EVENT_TAXONOMY` | dotted string (`hitl.requested`, `reasoning.start`, …) | Canonical `events.jsonl` rows and inner `AgentEvent` envelopes |

`EventCategory`: `pipeline`, `plan`, `tool`, `slot`, `clarification`,
`execution`, `llm`, `agentic`, `hitl`, `agent`, `task`, `feed`,
`observability`, `media`, `activity`.

`EventSeverity`: `info`, `warn`, `error`, `decision`, `attention`.

Adding a variant requires the same `taxonomies!` line in both
`realtime_events.rs` (exhaustiveness on `taxonomy_lookup`) and
`magician-event-taxonomy/src/lib.rs`.

`AgenticWaitingForUser` / `AgenticWaitingForConfirmation` are **Hitl /
attention / user_relevant**, not Agentic. `ChatMessageReceived`,
`ProgressEvent`, `ShellOutputChunk`, and `InteractivePtyChunk` are
Observability (the catch-all). Activity is its own category.

---

## Scope visibility

`event_visible_to_scope` (`realtime_events.rs`) is fail-closed and
exhaustive — rustc forces a decision for every new variant.

- Explicit `principal` / `workspace` → visible only to that pair.
- Missing scope on most variants → visible only to a consumer querying
  `system` / `system`.
- `Heartbeat` → visible to every connection (transport keepalive).
- `ProgressEvent` → compares the inner `ProgressMessage` scope.
- `FeedItemCreated` → inner `FeedItem` scope.
- `ThinkingMapUpdated` / `ThinkingMapInterpretProgress` → required
  (non-Option) scope, strict equality, no system bucket.
- `AgentEvent` → envelope scope, else payload `principal`/`workspace`,
  else `system`/`system`.

`RuntimeTransportBroadcaster::emit` / `emit_transport_only` run
`enrich_scope_if_registered`: a `HitlRequested` / `HitlResolved` with a
registered `execution_id` but missing principal/workspace/task_id is
filled from `CanonicalEventScope` at fan-out.

---

## Event persistence + retention

Two on-disk surfaces persist events; together they answer "what does a
refresh of `/events` return?".

### Per-execution canonical log

`…/scopes/{principal}/{workspace}/tasks/{task_id}/executions/{exec_id}/events.jsonl`
— written by `FilesystemRuntimeEventSink` for every `ArtifactV2EventType`
(116 variants). Dotted names (`llm.requested`, `tool.succeeded`,
`agentic.iteration_started`, `hitl.requested`, …). Retained for the
lifetime of the execution directory; not time-evicted.

- Sibling `events.jsonl.commit` (≤ 32 bytes) holds the durable committed
  byte length; backfill and execution-panel readers expose only that
  prefix. A suffix left by a failed write/flush/fsync is non-authoritative
  and truncated by the next writer. If the marker replacement is visible but
  directory sync and its retry fail, append returns `CommitUncertain` —
  never retry it as an ordinary failure (readers may already see the event).
- Ingress counts encoded bytes before queueing; an event over the 64 MiB
  record ceiling (`MAX_CANONICAL_EVENT_RECORD_BYTES`) is dropped.
- `ref_ids.output_ids` = payload `output_id` then `source_output_ids`,
  first-seen order, max 16,384 (`MAX_CANONICAL_EVENT_OUTPUT_REF_IDS`);
  above that the event is rejected.

Read by the `/events` per-execution backfill path when both
`execution_id` and `task_id` are pinned. No implicit `since` floor.

`map_v2_realtime_event` in `artifact_v2/events.rs` is the
RuntimeTransportEvent → ArtifactV2EventType mapping. `HitlRequested` /
`HitlResolved` persist as `hitl.requested` / `hitl.resolved`.

### Per-scope transport event log

`…/scopes/{principal}/{workspace}/events.jsonl` — written by
`transport_log::spawn_workspace_event_log_writer`. A single subscriber
drains every `RuntimeTransportEvent` off the broadcaster (capacity
`DEFAULT_RUNTIME_TRANSPORT_CAPACITY` = 8192) and routes it to the right
per-scope log. Captures broadcaster-only categories that do not go
through the canonical sink: `FeedItemCreated`, `ChatMessageReceived`,
`ExecutionPanelDelta`, `AgentEvent` envelopes, etc.

Routing follows `event_visible_to_scope`: explicit scope →
`(principal, workspace)`; missing scope → `system/system`. Path
components are sanitized (alphanumeric + `_-`, length-bounded); unsafe
values route to `_quarantine/_quarantine`.

**Retention** (hard-coded):

```text
EVENTS_RETENTION_WINDOW_MS = 24 * 60 * 60 * 1000   // rolling 24h
EVENTS_RETENTION_MIN_COUNT = 2000                   // newest-N floor
COMPACTION_INTERVAL        = 5 minutes
```

Every 5 minutes the compactor rewrites each log to keep
`max(events_in_24h, EVENTS_RETENTION_MIN_COUNT)` newest rows. Atomic via
tmp + rename; the per-log write mutex serializes append vs compact. Quiet
workspaces keep the last 2000 events even if older than 24h. Busy
workspaces keep everything within 24h. File opens retry briefly on
`EMFILE` / `ENFILE`. The compactor skips files ≤
`EVENTS_RETENTION_MIN_COUNT * 16` bytes and streams larger ones line by
line rather than loading them whole.

If the writer lags (`RecvError::Lagged`), it appends a
`__transport_log_lagged__` sentinel (`skipped`, `timestamp_ms`,
`message`) into `system/system` and logs at `warn!`.

Read by the `/events` cross-scope backfill path. The `since` floor is
**not** re-applied on the per-scope log read — retention is the source of
truth for how far back to look.

### Frontend dedup

The same logical event can land in both logs (canonical sink **and**
broadcaster subscriber). The two representations are not byte-identical:
canonical rows use dotted `ArtifactV2EventType` names; live tail and the
per-scope log serialize the PascalCase enum. `EventStreamCard` dedupes on
the serialized NDJSON line, which collapses identical copies (live ∩
transport-log overlap, which `serialize_event_if_passes` keeps
byte-identical) but not the two mapped forms.

`persist_execution_outcome` emits `TaskUpdated` when `outcome.is_terminal`
so task-list UIs refresh on run completion without waiting for a
shape-matched `FeedItemUpdated`.

---

## Event categories

Counts are live `EVENT_TAXONOMY_TABLE` rows (107 total). Retired HITL
aliases are not counted.

| Category | Count | Variants |
|----------|------:|----------|
| [Pipeline](#pipeline) | 11 | MessageProcessingStarted, QueryAnalysisCompleted, StrategySelected, ExplorationProgress, MessageCompleted, ProcessingError, Pipeline{Started,StepStarted,StepCompleted,Completed,Failed} |
| [Plan](#plan) | 8 | AtomicPlan{OutlineStarted,OutlineCompleted,ExpansionStarted,Generated}, V3Planning{Started,Progress,Completed,Failed} |
| [Tool matching](#tool-matching) | 2 | ToolMatchingTierStarted, ToolMatchingTierCompleted |
| [Slot](#slot-extraction) | 14 | Slot{ExtractionStarted,Extracted,EnrichmentStarted,EnrichmentCompleted,ConfidenceUpdated,GraphDiff}, ClarifiedTaskReady, Parameter{InferenceAttempted,Inferred,InferenceFailed,DiscoveryAttempted,Discovered,DiscoveryFailed,ResolutionProgress} |
| [Clarification](#clarification) | 3 | Clarification{SessionSnapshot,ConfidenceSnapshot,MetricsSnapshot} |
| [Execution](#execution-lifecycle) | 16 | Execution{Started,StepStarted,StepCompleted,Paused,Resumed,Failed,Cancelled,Completed,InflightResent,InflightDropped,RestoreFailed,StatusChanged,ResponsibilityChanged}, Workflow{Resumed,StageResumed,ResumeFailed} |
| [LLM](#llm-analysis) | 8 | LLMAnalysis{Started,Completed,Failed}, LLMRequestSent, LLMResponseReceived, InferenceAttempted, ThinkingMode{Activated,Completed} |
| [Agentic](#agentic-execution) | 17 | Agentic{ExecutionStarted,ExecutionCompleted,IterationStarted,IterationCompleted,StepStarted,StepCompleted,StepFailed,StepStuckWarning,PageUnderstanding,DecisionMade,ActionExecuted,ClickFallbackUsed,MaxIterationsReached,Resumed}, DomChangeDetected, SubGoal{Requested,Outcome} |
| [HITL](#human-in-the-loop-canonical) | 4 | AgenticWaitingForUser, AgenticWaitingForConfirmation, HitlRequested, HitlResolved |
| [Agent](#agent-lifecycle) | 5 | AgentCycle{Started,Completed}, AgentTriggered, AgentEvent, AgentDefinitionChanged |
| [Task](#task-crud) | 3 | Task{Created,Updated,Deleted} |
| [Feed](#feed) | 4 | FeedItem{Created,Updated,Removed}, ExecutionPanelDelta |
| [Observability](#observability) | 8 | Heartbeat, ObservabilityAlert, ChatMessageReceived, ThinkingMap{Updated,InterpretProgress}, ProgressEvent, ShellOutputChunk, InteractivePtyChunk |
| [Activity](#activity) | 4 | Activity{Started,Finished,Progress,Cost} |

Unless noted, variants carry optional `principal` / `workspace` and
`timestamp: i64` (unix ms). `skip_serializing_if = Option::is_none` omits
absent options.

---

## Pipeline

| Event | Payload |
|---|---|
| MessageProcessingStarted | `execution_id`, `turn_id`, `correlation_id`. |
| QueryAnalysisCompleted | `execution_id`, `correlation_id`, `complexity_score: f64`, `intent`, `categories: string[]`. |
| StrategySelected | `execution_id`, `correlation_id`, `strategy: StrategyType`, `confidence`, `reason`. |
| ExplorationProgress | `execution_id`, `correlation_id`, `nodes_explored`, `current_depth`, `best_score`, `current_task`, `progress_percent`. |
| MessageCompleted | `execution_id`, `turn_id`, `correlation_id`, `response`, `exploration_summary?`. |
| ProcessingError | `execution_id`, `correlation_id`, `error_message`, `error_type`. |
| PipelineStarted / PipelineCompleted / PipelineFailed | Ask-loop planning pipeline. Shared: `workflow_id`, `chain_id`. Started adds `max_iterations`. Completed/Failed add `steps_executed`. Failed adds `reason`. |
| PipelineStepStarted / PipelineStepCompleted | `workflow_id`, `step_id`, `agent_id`. Completed adds `outcome_kind`. StepStarted exists so subscribers can measure per-step latency without inferring it from the previous Completed. |

---

## Plan

| Event | Payload |
|---|---|
| AtomicPlanOutlineStarted | `execution_id`, `correlation_id`, `total_atomic_tools`. |
| AtomicPlanOutlineCompleted | `execution_id`, `correlation_id`, `goals_count`, `confidence`. |
| AtomicPlanExpansionStarted | `execution_id`, `correlation_id`, `goals_from_outline`. |
| AtomicPlanGenerated | `execution_id`, `correlation_id`, `turn_id`, `plan_graph: PlanGraph`, `validation_status` (`validating` / `valid` / `retrying`), `attempt_number`. |
| V3PlanningStarted / V3PlanningProgress / V3PlanningCompleted / V3PlanningFailed | Task-scoped planner. Required `principal`, `workspace`, `task_id`, `task_title`, `agent_id`, `plan_id`, `ui_thread_id`. Progress adds `phase`, `detail?`. Failed adds `error`. Planning HITL questions ride `HitlRequested { source: "clarification" }`, not a V3PlanningClarification variant. |

---

## Tool matching

| Event | Payload |
|---|---|
| ToolMatchingTierStarted | `execution_id`, `correlation_id`, `tier_number` (0–4), `tier_name` (Category Pre-Filter / Rule-Based / Semantic / Candidate Selection / LLM Evaluation), `description`. |
| ToolMatchingTierCompleted | Same ids plus `candidates_count`, `duration_ms`, `top_candidates: TierCandidate[]` (`tool_name`, `score`, `category`). |

---

## Slot extraction

| Event | Payload |
|---|---|
| SlotExtractionStarted | `execution_id`, `correlation_id`, `message_length`. |
| SlotExtracted | `execution_id`, `correlation_id`, `slot_id`, `slot_type`, `confidence`. |
| SlotEnrichmentStarted | `execution_id`, `correlation_id`, `total_slots`, `enricher_count`. |
| SlotEnrichmentCompleted | `execution_id`, `correlation_id`, `total_slots`, `slots_changed`, `invocations`, `errors_count`. |
| SlotConfidenceUpdated | `execution_id`, `correlation_id`, `slot_id`, `old_confidence`, `new_confidence`, `source` (`LlmPrimary`, `UserReply`, `DeterministicCheck`, …). |
| SlotGraphDiff | `execution_id`, `source`, `inserted`, `updated`, `removed`, `total_slots`. |
| ClarifiedTaskReady | `execution_id`, `correlation_id`, `clarified_task`, `objectives_count`, `constraints_count`, `confidence`. |
| ParameterInferenceAttempted / ParameterInferred / ParameterInferenceFailed | `execution_id`, `parameter_name`. Inferred adds `inferred_value`, `confidence`, `method` (`LLMBased` / `RuleBased` / `Historical` / `Default` / `AutoFill`). Failed adds `confidence`, `reason`. Attempted adds `priority`. |
| ParameterDiscoveryAttempted / ParameterDiscovered / ParameterDiscoveryFailed | `execution_id`, `parameter_name`. Attempted/Discovered add `discovery_method`. Discovered adds `discovered_value`, `confidence`, `external_actions_performed`. Failed adds `reason`. |
| ParameterResolutionProgress | `execution_id`, `total_parameters`, `resolved_count`, `inferred_count`, `discovered_count`, `deferred_count`, `remaining_count`. |

---

## Clarification

Session telemetry only. The human-response payload is `HitlRequested` /
`HitlResolved` with `source: "clarification"`.

| Event | Payload |
|---|---|
| ClarificationSessionSnapshot | `execution_id`, `state`, `total_questions`, `waiting_on_user`, `queued`, `answered`, `cancelled`, `pending_question_ids: string[]`, `active_batch?`, `last_question_asked_at?`. |
| ClarificationConfidenceSnapshot | `execution_id`, `question_id`, `trigger`, `overall_confidence`, `unresolved_count`, `slot_deltas`, `question_created_at`, `answered_at`. |
| ClarificationMetricsSnapshot | Process-wide (no execution_id): `total_sessions_started`, `total_sessions_completed`, `active_sessions`, `avg_session_duration_ms?`, `avg_questions_per_session?`, `guardrail_timeouts`, `guardrail_question_caps`, `guardrail_round_caps`. |

---

## Execution lifecycle

| Event | Payload |
|---|---|
| ExecutionStarted | `execution_id`, `plan_id`, `steps_total`. |
| ExecutionStepStarted | `execution_id`, `plan_id`, `step_index`, `step_id`, `steps_total`. |
| ExecutionStepCompleted | `execution_id`, `plan_id`, `step_index`, `step_id`, `success`. |
| ExecutionPaused | `execution_id`, `plan_id`, `step_index`, `step_id`, `reason`. Also the reconnect re-emit for a manual pause. |
| ExecutionResumed | `execution_id`, `plan_id`, `step_index`, `step_id?`, `mode`. |
| ExecutionFailed | `execution_id`, `plan_id`, `step_index`, `step_id`, `error`. |
| ExecutionCancelled | `execution_id`, `plan_id`, `step_index`. |
| ExecutionCompleted | `execution_id`, `plan_id`, `steps_total`, `success`. |
| ExecutionInflightResent | `execution_id`, `plan_id`, `step_index`, `step_id`, `request_id`, `attempt_count`. |
| ExecutionInflightDropped | Same ids without `attempt_count`. |
| ExecutionRestoreFailed | `execution_id`, `reason`, `note?`. |
| ExecutionStatusChanged | `execution_id`, `task_id?`, `root_execution_id?`, `previous_status`, `new_status`, `reason?`. |
| ExecutionResponsibilityChanged | `execution_id`, `parent_execution_id?`, `task_id?`, `root_execution_id?`, `waiting_state`, `active_owner_agent_id`, `owner_stack: string[]`, `active_delegation_group: string[]`. |
| WorkflowResumed | `execution_id`, `question_id?`, `resume_mode`, `answered_count`, `pending_count`. |
| WorkflowStageResumed | `execution_id`, `stage_name`, `stage_context`, `attempt`, `reused_checkpoint`, `checkpoint_hash?`, `reused_stages: string[]`. |
| WorkflowResumeFailed | `execution_id`, `question_id?`, `error`. |

When a failed `agentic-continue` restores a pause and re-emits
`AgenticWaiting*`, the emitted `execution_id` is the effective requested
execution. Treat that event `execution_id` as canonical.

---

## LLM analysis

### LLMAnalysisStarted / LLMAnalysisCompleted / LLMAnalysisFailed
Planner-side LLM tracing. Shared: `execution_id`, `correlation_id`,
`provider`, `stage`. Started adds `query_length`. Completed adds
`response_length`, `duration_ms`. Failed adds `error_type`
(`api_key_missing` / `timeout` / `invalid_response` / `network_error`),
`error_message`.

### LLMRequestSent
Agentic-loop request. `execution_id`, `plan_id`, `step_id?`,
`step_index?`, `capability` (`page_understanding`, `decision`, …),
`request_summary`, `input_tokens_estimate?`, `budget_remaining`.

### LLMResponseReceived
Agentic-loop response. Distinctive fields:

| Field | Type | Notes |
|-------|------|-------|
| `correlation` | `LlmEventCorrelation?` | Canonical recorder input. `correlation.activity_id` joins the call to the span (and thus `ActivityCost`). Absent for calls outside an instrumented span |
| `capability` | string | e.g. `decision` |
| `success` | bool | |
| `decision_summary` | string | |
| `cost` | f64 | USD via `magicllm::pricing`; `0.0` when no pricing row matches |
| `latency_ms` | u64 | |
| `error` | string? | |
| `provider` | string | lowercase (`anthropic`, `minimax`, `openai`, `openrouter`, `gemini`, `ollama`, `yutori`) |
| `model` | string | effective model id |
| `usage_reported` | bool | zero token buckets are meaningful only when true |
| `input_tokens` / `output_tokens` / `reasoning_tokens` | u32 | |
| `cache_read_tokens` / `cache_creation_tokens` | u32 | |
| `audio_input_tokens` / `audio_output_tokens` / `audio_cached_tokens` | u32? | realtime voice only |
| `search_calls` | u32 | `server_web_search` lane |
| `ttft_ms` | u64? | time-to-first-token |
| `task_id` / `agent_id` / `delegated_agent_id` / `chat_session_id` | string? | |
| `operation` | string | e.g. `agentic_decision` |
| `profile` | string? | selected LLM profile |
| `attempt` | u32 | 1-based |
| `response_kind` | string | `tool_call` / `text` / `streaming` / `reasoning_only` / `error` |
| `started_at_ms` | i64 | 0 when unmeasured |
| `reasoning_summary` | string? | provider chain-of-thought summary when requested |

`llm_trace_activation` rebuilds `LlmTraceContext` from `correlation` and
nothing else.

### InferenceAttempted
Agentic parameter inference: `execution_id`, `plan_id`, `step_id?`,
`parameter`, `inferred_value?`, `confidence`, `reason`, `accepted`.

### ThinkingModeActivated / ThinkingModeCompleted
Adaptive-profile escalation. Shared: `execution_id`, `chat_session_id?`,
`chat_turn_id`, `adaptive_profile`. Activated adds `fast_profile`,
`thinking_profile`, `reason?`. Completed fires at turn end so the UI can
clear the chip.

---

## Agentic execution

Observe-decide-execute loop. Pause **payload** is on `HitlRequested`; the
`AgenticWaiting*` variants below are slim lifecycle markers (Hitl
category).

### AgenticExecutionStarted
`execution_id`, `plan_id`, `step_id`, `goal`, `success_criteria`,
`max_iterations`, `hint_action?`, `agent_id?`.

### AgenticIterationStarted
`execution_id`, `plan_id`, `step_id`, `iteration` (1-indexed),
`environment_type` (`browser` / `filesystem` / `http` / `shell`).

### AgenticIterationCompleted
Emitted at the end of each iteration (action executed, decision rejected,
or fall-through) so subscribers can measure duration without waiting for
the next Started. `duration_ms`, `outcome`: `action_executed` /
`decision_rejected` / `loop_continue` / `terminal` / `paused` /
`cancelled`.

### AgenticStepStarted / AgenticStepCompleted / AgenticStepFailed
Taskplan step lifecycle. The decision LLM has no explicit "step start"
signal; the runtime synthesizes Started immediately before each
Completed/Failed, tracked via `started_step_ids` so each step emits
exactly one Started per execution. Fields: `execution_id`, `plan_id`,
`step_id`, `iteration`.

### AgenticStepStuckWarning
Observability-only; no behavior change. `iteration`, `is_preflight`
(ignore as a stuck signal when true — capability PREFLIGHT blocker),
`consecutive_count`, `recent_actions` (up to last 3 `action_summary`
strings).

### AgenticPageUnderstanding
`observation_id`, `iteration`, `page_stage`, `element_count`,
`appears_loading`, `url?`, `confidence`, `has_screenshot`.

### AgenticDecisionMade
`iteration`, `decision_type` (`execute`, `goal_reached`, `cannot_proceed`,
`need_user_input`, `spawn_sub_goal`, `delegate_to_agent`,
`delegate_to_agent_async`, `create_task`), `action_summary?`, `reasoning`,
`confidence`, `thinking?`, `evidence?`, `tool_name?`, `action_type?`,
`element_id?`, `candidates_count?`, `raw_decision?`.

### AgenticActionExecuted
`iteration`, `action_type`, `target`, `success`, `latency_ms`, `error?`.
Emitted for every executed action, failed ones included: a provider error or
a result the executor classifies as unsuccessful carries `success: false` and
a bounded `error`, with the same `action_type`/`target` identity a success
would have. Canonical projection: `tool.succeeded` / `tool.failed`.

### AgenticClickFallbackUsed
Legacy coordinate-fallback telemetry. Current browser automation returns
failures to the visible agent loop instead. `original_selector`,
`original_error`, `coordinates: (f64, f64)`, `fallback_success`,
`fallback_error?`, `latency_ms`.

### AgenticExecutionCompleted
`outcome`, `iterations_used`, `artifacts: string[]`, `duration_ms`,
`summary`. Outcomes: `success`, `failed`, `max_iterations_reached`,
`loop_detected`, `waiting_for_user`, `waiting_for_confirmation`,
`budget_exhausted`, `cannot_proceed`. Conditional fields:
`loop_detected` → `loop_detection_type`, `loop_repeated_action`,
`loop_recommendation`, `loop_cycle_pattern`, `loop_similarity`;
`budget_exhausted` → `budget_dimension`, `budget_details`;
`cannot_proceed` → `cannot_proceed_reason`. Also `refinement_pass_index`
(0 = original), `refinement_pending`, `yield_payload?` (structured Yield
termination).

### AgenticMaxIterationsReached
Slim lifecycle marker. `execution_id`, `plan_id`, `step_id`,
`iterations_used`, `pause_state_id?`, `agent_id?`, `goal_id?`,
`cycle_id?`. Prompt lives on `HitlRequested { source: "agentic",
input_schema.pause_kind: "max_iterations" }`.

### AgenticWaitingForUser
Slim lifecycle marker. HITL fields (`question`, `input_type`, `hint`,
`options`, `previous_answer`, `retry_reason`) are **not** on this
variant; they ride `HitlRequested { source: "agentic" }` from
`emit_hitl_requested_for_agentic_pause`.

| Field | Type | Description |
|-------|------|-------------|
| `execution_id` | string | Execution UUID |
| `plan_id` | string | Plan UUID |
| `step_id` | string | Step UUID |
| `iteration` | usize | Iteration when paused |
| `pause_state_id` | string? | Resume routing id; reconnect dedupe key |
| `correlation_id` | string? | Set only for `diff_approval` pauses (`ccp-<uuid>`); `None` otherwise |
| `is_retry` | bool? | Re-ask after rejection |
| `retry_count` | usize? | Previous attempts |
| `agent_id` / `goal_id` / `cycle_id` | string? | Agent routing |
| `escalation_trigger` | string? | `cannot_proceed` / `loop_detected` when `on_failure: ask_user`; `None` for ordinary input |

### AgenticWaitingForConfirmation
Slim lifecycle marker. `action_summary` / `reason` / `action_type` are
**not** on this variant; they ride `HitlRequested { source: "agentic" }`
from `emit_hitl_requested_for_agentic_confirmation`.

| Field | Type | Description |
|-------|------|-------------|
| `execution_id` | string | Execution UUID |
| `plan_id` | string | Plan UUID |
| `step_id` | string | Step UUID |
| `iteration` | usize | Iteration when paused |
| `pause_state_id` | string? | Resume routing id |
| `agent_id` / `goal_id` / `cycle_id` | string? | Agent routing |

### AgenticResumed
`pause_state_id?`, `plan_id`, `step_id`, `resumed_from_iteration`,
`input_type`, `user_responded`, plus agent routing ids.

### DomChangeDetected
magicutor SSE backchannel. `correlation_id`, `total_changes`,
`nodes_added`, `nodes_removed`, `signals: string[]`, `initial_url?`,
`final_url?`, `action_type?`, `selector?`, `outcome` (`success` /
`failed`).

### SubGoalRequested / SubGoalOutcome
`execution_id`, `plan_id`, `parent_step_id`, `sub_goal`. Requested adds
`budget_iterations`, `depth`. Outcome adds `outcome`, `iterations_used`,
`duration_ms`.

---

## Human-in-the-Loop (Canonical)

Every HITL surface emits `HitlRequested` when a prompt becomes pending
and `HitlResolved` when it is answered, expires, is cancelled, or is
dismissed. Agentic pauses **also** emit the slim `AgenticWaiting*` /
`AgenticMaxIterationsReached` lifecycle markers. Approvals also publish
`agent.update` `approval_requested`. New consumers should subscribe to
the canonical pair and dispatch on `source`.

`pendingHitlStore` is authoritative for "how many things is the system
waiting on a human for".

Pending/resolved authority survives restart through the private keyed
`pending_hitl_lifecycle.v3.sqlite3` store. Ready startup validates the
schema, private files and downgrade fence and seeds app expiry from one
indexed minimum query (no aggregate replay). Exact reads and transitions
use one scoped primary key. The old `pending_hitl_lifecycle.jsonl` is
consumed only by a one-time fixed-size streaming import (capped records,
byte/depth/node admission) that commits before readiness; a durable
directory fence stops an older binary from reopening split authority. New
lifecycle records and `input_schema` values are admitted before
serialization (rejected trees drained iteratively). Unscoped legacy HITL
gets the same structural and encoded-byte ceilings before fan-out.

Privacy exception: the host-sealed app-owner-notification family is
consumed at the broadcaster boundary before the shared in-process ring;
owner surfaces read the scoped durable `UserRequestService` record
directly, and generic subscribers and `pendingHitlStore` never receive its
prompt body. Its pending rows are content-free (`body = NULL`, only expiry
plus an opaque generation binding the live UserRequest, no body digest); a
resolved row may keep the fingerprint of its content-free `HitlResolved`
envelope, which cannot reconstruct or authorize the body.

Respond path: `POST /api/magician/v2/hitl/{correlation_id}/respond`.
`source` selects the arm:

| `source` | Dispatch |
|----------|----------|
| `agentic`, `primitive`, `inner_loop`, `escalation` | Agentic pause resume (`is_agentic_runtime_hitl_source`) |
| `user_request` | `UserRequestService::respond_scoped` |
| `approval` | `resolve_approval` |
| `clarification` | V3 task-plan clarification, then ask-loop |
| `bot_auth` | Bot auth HITL broker |
| `mcp_oauth` | MCP OAuth API |

`progress_channel_seam/normalize.rs` projects canonical HITL onto chat/feed
`AgentNotification` rows. `feed/materializer.rs` reads `metadata.source`
to bucket `FeedItemType`. Per-row attention_kind labels stay surface-side.

The envelope mirrors frontend `HitlRequest` in
`ui/unified-ui/src/lib/hitl/types.ts`.

### HitlRequested

| Field | Type | Description |
|-------|------|-------------|
| `correlation_id` | string | Stable id for the request lifetime (`pause_state_id` / `request_id` / `approval_id` / `question_id`) |
| `source` | string | `agentic`, `primitive`, `inner_loop`, `user_request`, `approval`, `clarification`, `escalation`, `bot_auth`, `mcp_oauth` |
| `input_type` | string | `text`, `password`, `otp`, `choice`, `multi_choice`, `confirmation`, `external_action`, `file_path`, `guidance`, `tool_authorization`, `sandbox_override`, `diff_approval`, `form` |
| `prompt` | string | Question, summary, or escalation banner |
| `hint` | string? | Supplementary hint |
| `input_schema` | object? | Typed schema (`placeholder`, `options`, `multiline`, `allow_other`, `min_selections`, `max_selections`) plus `sensitive` — the value-free `SensitiveInputSpec` (`kind`, `fields[]`, `provenance`, `one_time`, `collection_deadline_ms`) when the ask collects a secret, published for the `user_request` and `agentic` sources so a client masks by it (see [Sensitivity contract](hitl-attention.md#sensitivity-contract-secure-hitl-credentials-p1)) — plus per-source extensions: clarification `stage` / `blocker_type` / `urgency` / `source_slot_id` / `chain_id` / `chain_position` / `chain_total`; approval `approval_id` / `goal_id` / `cycle_id` / `trigger_seq` / `pending_action_count` / `expires_at` |
| `task_id` / `execution_id` / `agent_id` | string? | Scope when available |
| `principal` / `workspace` | string? | Required for visibility filtering; `system` for purely system-emitted prompts |

### HitlResolved

| Field | Type | Description |
|-------|------|-------------|
| `correlation_id` | string | Same id as the originating `HitlRequested` |
| `source` | string | Same as the request |
| `outcome` | string | `responded`, `expired`, `cancelled`, `dismissed` |
| `decision` | string? | Opaque per source (`approve` / `reject`, `confirm` / `deny`, …) |
| `task_id` / `execution_id` / `agent_id` / `principal` / `workspace` | string? | Same scoping rule as the request |

---

## Agent lifecycle

| Event | Payload |
|---|---|
| AgentCycleStarted | `agent_id`, `goal_id`, `cycle_id`, `execution_id?`, `goal`. |
| AgentCycleCompleted | Same ids plus `outcome` (`success` / `failure` / `paused` / `cancelled`), `iterations_used`. |
| AgentTriggered | `agent_id`, `goal_id`, `trigger` (`manual` / `schedule` / `webhook` / `event`). |
| AgentEvent | `event: AgentEventEnvelope` — `event_type` (dotted GAUI / operator string), `agent_id`, optional `principal` / `workspace`, `payload`, `timestamp`. Carries `reasoning.*`, `tool.call.*`, `plan.*`, `agent.update`, and other GAUI rows that are not first-class enum variants. See [Reasoning & Tool-Call Streaming](#reasoning--tool-call-streaming) and [Operator Feed](#operator-feed-agent-update). |
| AgentDefinitionChanged | Required `principal`, `workspace`, `agent_id`. Signals session-scoped chat lifecycle subscriptions to re-materialize. |

---

## Task CRUD

Required `principal`, `workspace`, `task_id`; optional `execution_id`,
`ui_thread_id`; `title`. Created also has `created_at` / `updated_at`.
Updated has `updated_at`. Deleted has `deleted_at`.

---

## Feed

| Event | Payload |
|---|---|
| FeedItemCreated | `item: FeedItem` plus top-level `timestamp` (the inner `item.created_at` is not what `extractTimestamp` reads). |
| FeedItemUpdated | `principal`, `workspace`, `id`, `task_id?`, `ui_thread_id?`, `execution_id?`, `patch: FeedItemPatch`, `timestamp`. |
| FeedItemRemoved | Same identity fields without `patch`. |
| ExecutionPanelDelta | `principal`, `workspace`, `task_id?`, `execution_id?`, `state: ExecutionPanelState`. Full curated panel snapshot, not a patch. |

---

## Chat sessions

### ChatMessageReceived
`session_id`, `message: ChatMessage` (same shape as the chat REST API),
`origin_channel?`. Consumed by the web chat UI and consumer-channel bots
on a persistent WebSocket. Without
`supports_structured_presentation=true`, `message.presentation` is
stripped before send.

---

## Observability

### ObservabilityAlert
`execution_id`, `alert_type`, `details`.

### Heartbeat
`timestamp` only. Visible to every WS scope. Connection-ready signal and
post-lag keepalive on `/realtime/ws` (no `skipped` field).

### ProgressEvent
`message: ProgressMessage` (progress_channel_seam). The progress router
subscribes and fans out; producers emit only onto this bus.
`ActivityProgress` is unrelated — that is a tracing log line.

### ThinkingMapUpdated
Required scope. Lightweight change notice (`map_id`, `revision`) — clients
re-fetch `GET /thinking-maps/{id}`. No map payload on the wire.

### ThinkingMapInterpretProgress
`map_id`, `utterance_id`, `stage` (`preparing` / `loading_context` /
`facilitating` / `parsing` / `shaping` / `idle`; tolerate unknown),
`detail?`, `node_count?`. Best-effort narration; the result still arrives
as `ThinkingMapUpdated` plus the HTTP response.

### ShellOutputChunk
Batched every 100ms from `execute_bash_action`. `execution_id`, `step_id`,
`step_index`, `command` (first chunk only), `stream` (`stdout` /
`stderr`), `data`, `sequence`, `is_final`, `exit_code?` (final only).

### InteractivePtyChunk
Live PTY bytes for the xterm pane. Required `principal` / `workspace`.
`session_id`, `ui_thread_id?`, `program?`, `offset_start`, `offset_end`,
`bytes_b64` (decode before `term.write()`), **`timestamp_ms`** (not
`timestamp`).

---

## Reasoning & Tool-Call Streaming

These are **not** `RuntimeTransportEvent` variants. They are GAUI strings
emitted via `RuntimeTransportBroadcaster::emit_named` into `AgentEvent`
envelopes and classified by `GAUI_EVENT_TAXONOMY` (`reasoning.*` → Llm,
`tool.call.*` → Tool).

All five carry `execution_id` in the payload so the chat-side fan-out
registry (keyed on `payload.execution_id`) can re-stamp delegated events
into a parent chat thread. Without `execution_id`, only
`reasoning.start` and `tool.call.started` would reach chat.

### reasoning.start
`trace_id`, `execution_id` (null from chat outer loop), `model?`,
`budget_tokens?`, `started_at`.

### reasoning.content
`trace_id`, `execution_id`, `delta`. Governed app turns omit content after
their monotonic persistence fence while preserving start/end lifecycle
metadata.

### reasoning.end
`trace_id`, `execution_id`, `total_tokens?`, `duration_ms?`, `ended_at`.

### tool.call.started
`call_id`, `tool_name`, `execution_id`, `step_id?`, `args?` (governed app
calls expose only an omission marker, canonical digest, and bounded
shape), `started_at`.

### tool.call.args
`call_id`, `execution_id`, `delta`.

### tool.call.finished
`call_id`, `tool_name`, `execution_id`, `success`, `duration_ms?`,
`content_preview?` (≤240 chars), `exit_code?`, `error?`, `finished_at`.

### Chat fan-out for delegated executions

When a chat session has registered a `chat_fanout` target for an
`execution_id` (via `delegate_to_agent`), every event in this section
gets a stamped copy under the chat agent's `agent_id+scope`:

| Stamped field | Value |
|---------------|-------|
| `chat_delivery_kind` | `"inline_delegate"` |
| `chat_session_id` | The chat session that registered the fan-out |
| `origin_agent_id` | The delegate's agent_id |
| `chat_turn_id` | Spawning turn, when the session supplied one |
| `call_id` (on `tool.call.*`) | `delegate-{execution_id}/{orig_call_id}` |
| `origin_call_id` (on `tool.call.*`) | The unrewritten `call_id` |

Fan-out is skipped when target identity equals primary (chat agent
self-delegating). The original envelope under the delegate's
`agent_id+scope` is still emitted.

---

## Activity

Unified runtime activity (`EventCategory::Activity`). Emitted by the
tracing layer (`runtime_activity_layer.rs`), not by hand: every
instrumented span becomes a started/finished pair, and every INFO-or-above
log line becomes a progress row. UI:
[`runtime-activity-view.md`](../unified-ui/runtime-activity-view.md).

A span that declares no scope, and inherits none from a tracked ancestor,
falls back to **`anonymous` / `default`** — not `system` / `system`.
Genuinely cross-scope passes declare `system` / `system` positively
(e.g. `attention_rank_recompute_pass`). An Activity event whose
`principal`/`workspace` are still `None` on the wire is fail-closed to
`system`/`system` by `event_visible_to_scope`.

`kind` is normalised onto `agent` / `background` / `capability` / `llm` /
`process` / `runtime`. `outcome` onto `success` / `error` / `cancelled` /
`closed`. `message` is the one free-form field — emit sites must keep
credentials, prompt bodies, tool arguments, and user content out of it.
The layer caps it at 2 KiB.

The layer writes into a bounded queue (4096) and, when full, evicts the
oldest record. Every variant carries `dropped`: process-cumulative count
of evicted records, stamped when drained. `seq` is a decimal string
(counter seeded from a 63-bit random base) — deduplicate on this; a JSON
number past 2^53 loses precision in a browser.

The layer is registered in `init_tracing` with its own per-layer filter
pinned to the process baseline, not `--log-level`. Activity rows keep
arriving even when the console is quiet; `dropped` is the only way this
stream is incomplete.

The five dimension fields — `workload_class`, `agent_id`, `thread_id`,
`task_id`, `model` — are inherited from the nearest tracked ancestor.
A declared value always wins. They are omitted from the payload when
neither declared nor inherited. Unlike principal/workspace they have
**no default**. `operation` does not inherit.

### ActivityStarted
`activity_id`, `parent_activity_id?`, `name`, `target`, `kind`,
`principal?`, `workspace?`, `workload_class?` (`foreground_chat` /
`interactive_task` / `ambient` / `scheduled` / `autonomous_task` /
`memory` / `system` / `evaluation` / `comms_assist` — byte-for-byte
`LlmWorkloadClass::as_str()`), `agent_id?`, `thread_id?`, `task_id?`,
`model?`, `operation?` (the `LLMOperation` name; present only on spans
that declare it), `dropped`, `seq`, `timestamp`.

### ActivityFinished
`activity_id`, `duration_ms`, `outcome`, same scope + `dropped` + `seq`.
`closed` is the default when nothing was declared.

### ActivityProgress
`activity_id?` (absent below the span floor — parentless rows render at
the root), `level`, `message`, `target`, scope, `dropped`, `seq`.

### ActivityCost
Published by the canonical LLM recorder **after** the submitting span
has closed. Attach it to a span the consumer already holds; a cost whose
span is gone is dropped, not parked at the root.

Not every span gets one: no `activity_id` → nothing; unpriced call →
nothing (not zero); span that made no model call → nothing. A span can
receive more than one cost (several model calls, possibly different
commodities) — accumulate per commodity, never add across them.

| Field | Type | Description |
|-------|------|-------------|
| `activity_id` | string | Matching Started/Finished pair |
| `cost_microunits` | u64 | Millionths of one unit of `commodity` |
| `commodity` | string | Required. `usd` for vendor-priced calls; `local` for operator hardware; package-specific units (`tavily_credit`, …) |
| `input_tokens` / `output_tokens` | u64? | When the provider reported them |
| `dropped` | u64 | Always `0` on this variant — it is not produced by the layer queue. Receiver-side loss surfaces as `__events_lagged__` |
| `seq` | string | Logical `llm_call_id` |

---

## Operator Feed (Agent Update)

Typed semantic events for the `/feed` operator timeline. See
[agent-updates.md](./agent-updates.md).

`publish_agent_update` wraps an `AgentUpdate` in `AgentEventEnvelope`
with inner `event_type = "agent.update"` and emits
`RuntimeTransportEvent::AgentEvent`. On `/realtime/ws` the outer tag is
therefore `AgentEvent`; the operator `kind` discriminator lives at
`data.event.payload.kind`. `/events?event_type=agent.update` matches
because it unwraps the inner type.

### agent.update (envelope)

| Envelope field | Type | Description |
|---|---|---|
| `event_type` | string | Always `"agent.update"` (inner envelope) |
| `agent_id` | string | Owning agent id |
| `principal` | string? | Scope principal (required for journal routing) |
| `workspace` | string? | Scope workspace (required for journal routing) |
| `timestamp` | i64 | Envelope timestamp (ms) |

**Payload** (common fields on every variant): `id` (ULID), `ts`,
`workspace_id`, `agent_id`, `thread_id?`, `cycle_id?`, `kind`.

**Variants (30):** `AgentUpdateKind` in
`magician/src/magician_v2/agents/agent_update.rs`.

| `kind` | Purpose | Variant fields |
|---|---|---|
| `agent_created` | New agent definition appeared | `name`, `agent_kind` |
| `agent_updated` | Agent definition changed | `changed_fields: string[]` |
| `agent_deleted` | Agent removed | — |
| `agent_paused` | Agent paused | `reason?` |
| `agent_resumed` | Agent resumed | — |
| `cycle_started` | Observe-decide-execute cycle began | `focus_area?`, `trigger` |
| `cycle_completed` | Cycle finished normally | `outcome` (`succeeded`/`failed`/`paused`/`partially_succeeded`), `duration_ms` |
| `cycle_failed` | Cycle errored | `error`, `duration_ms` |
| `cycle_paused` | Cycle paused mid-execution | `reason` |
| `goal_completed` | Goal fulfilled | `goal_id`, `artifacts[]` |
| `goal_failed` | Goal abandoned | `goal_id`, `error` |
| `goal_recovered` | Goal succeeded after prior failures | `goal_id`, `recovery_strategy` |
| `approval_requested` | Action awaiting human decision | `approval_id`, `tool?`, `action?`, `params?`, `pending_action_count?` |
| `approval_resolved` | Approval decided | `approval_id`, `decision` (`approve`/`reject`), `resolver` |
| `approval_expired` | Approval timed out | `approval_id` |
| `artifact_created` | File artifact persisted | `artifact` (`{artifact_id, kind, surface_url?, label?}`) |
| `artifact_create_failed` | Artifact persist failed | `attempted_kind`, `error` |
| `circuit_opened` | Breaker tripped | `scope`, `reason`, `next_retry_at?` |
| `circuit_recovered` | Breaker closed — only when `previous_failures >= max_consecutive_failures` | `scope`, `recovered_at` |
| `task_created` | Task entering `planning` or `ready` | `task_id`, `title` |
| `task_updated` | Task state progressed | `task_id`, `changed_fields[]` |
| `task_completed` | Task reached `completed` | `task_id`, `artifacts[]` |
| `task_failed` | Task reached `failed` or `cancelled` | `task_id`, `error` |
| `memory_report` | Memory consolidation summary | `summary`, `episodes_processed`, `corrections_applied` |
| `feedback_generated` | Feedback loop fired | `corrections`, `success_patterns` |
| `tier_consolidated` | Consolidation rule wrote to a target tier | `source`, `target`, `records_written`, `rule` |
| `delegation_issued` | Parent agent dispatched a child | `from_agent`, `to_agent`, `task_id` |
| `delegation_resolved` | Child execution terminated | `from_agent`, `to_agent`, `task_id`, `outcome` |
| `feed_stalled` | No events for a cycle past `stall_after` | `silent_for_ms` |
| `published_surface_changed` | Published surface updated | `surface_id`, `url` |

**Wire example** (`cycle_completed` inner envelope; WS wraps this in
`AgentEvent`):

```json
{
  "event_type": "agent.update",
  "agent_id": "cfo",
  "principal": "alpha",
  "workspace": "ws_a",
  "timestamp": 1745432100123,
  "payload": {
    "id": "01HXTEST000000000000000000",
    "ts": 1745432100123,
    "workspace_id": "ws_a",
    "agent_id": "cfo",
    "cycle_id": "cyc_42",
    "kind": "cycle_completed",
    "outcome": "succeeded",
    "duration_ms": 1234
  }
}
```

**Persistence:** `AgentUpdateJournal` appends every `agent.update`
envelope to `scopes/<principal>/<workspace>/updates.jsonl` (rotates at
50 MB). Events without principal/workspace are dropped.

**REST:** `GET /api/magician/v2/updates?since=&limit=&agent=&kind=&thread=`
with the workspace-bound bearer. Default `limit` 200, max 1000. Filters
apply before the limit. Reverse-chronological.

---

## Event flow

### Message processing

```
MessageProcessingStarted
  +-- QueryAnalysisCompleted
  +-- StrategySelected
  +-- ExplorationProgress (0..n)
  +-- ToolMatchingTierStarted/Completed (per tier)
  +-- AtomicPlanGenerated
  +-- ExecutionStarted
  |     +-- ExecutionStepStarted/Completed
  |     +-- AgenticExecutionStarted (if agentic)
  +-- ExecutionCompleted
MessageCompleted
```

### Agentic loop

```
AgenticExecutionStarted
  +-- AgenticIterationStarted
  |     +-- LLMRequestSent / LLMResponseReceived
  |     +-- AgenticPageUnderstanding (browser)
  |     +-- AgenticDecisionMade
  |     +-- AgenticActionExecuted
  |     +-- AgenticIterationCompleted
  +-- (if needs a human)
  |     +-- AgenticWaitingForUser / AgenticWaitingForConfirmation  (lifecycle)
  |     +-- HitlRequested { source: "agentic" }                    (payload)
  |     +-- HitlResolved / AgenticResumed
AgenticExecutionCompleted
```

---

## Related

- [`v2-api-guide.md`](./v2-api-guide.md#unified-event-stream--get-apimagicianv3events) — `/events` query contract
- [`agent-updates.md`](./agent-updates.md) — operator-feed architecture
- [`../unified-ui/runtime-activity-view.md`](../unified-ui/runtime-activity-view.md) — activity UI
- Historical specs: `../../archive/magician/AGENTIC_EXECUTION_OBSERVABILITY.md`, `../../archive/magician/EXECUTION_OBSERVABILITY.md`

