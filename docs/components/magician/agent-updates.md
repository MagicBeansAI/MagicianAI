# Agent Update Semantic Layer — Architecture

Typed, durable, operator-facing event timeline for all agent activity. The
backend emits ~55 distinct event strings (cycle, goal, circuit, approval,
artifact, task, memory, feedback, agent lifecycle), each with its own payload
shape; this layer folds them into one typed `agent.update` stream that the
`/feed` surface renders chronologically.

- **Design (archived):** `docs/archive/plans/2026-04-23-agentic-ui-semantic-layer.md`
- **Wire protocol reference:** [`v2-websocket-events.md`](./v2-websocket-events.md#operator-feed-agent-update)
- **Frontend surface:** [`../unified-ui/agent-updates-feed.md`](../unified-ui/agent-updates-feed.md)

## Shape

```
agent emission sites
  -> emit_* helpers + derived-event translator
  -> RuntimeTransportBroadcaster (event_type = "agent.update" envelopes)
       -> WebSocket clients -> agentUpdateStore (TS) -> /feed (30 typed cards)
       -> JSONL journal (rotating) -> GET /api/magician/v2/updates
       -> feed_stall_watchdog (synthesizes FeedStalled)
```

## Rust surface

### 1. `AgentUpdateKind` — the vocabulary

[`src/magician_v2/agents/agent_update.rs`](../../../magician/src/magician_v2/agents/agent_update.rs).
Internally-tagged serde enum (`#[serde(tag = "kind", rename_all = "snake_case")]`)
with 30 variants covering every operator-facing state transition. Each variant
carries only the fields the feed card needs; full detail stays on specialized
APIs (approval detail, task detail, etc.). The `AgentUpdate` envelope adds
`id` (ULID, time-sortable), `ts` (unix ms), `workspace_id`, `agent_id`, optional
`thread_id` / `cycle_id`, and flattens the kind body. Full variant table:
[`v2-websocket-events.md#operator-feed-agent-update`](./v2-websocket-events.md#operator-feed-agent-update).

### 2. `publish_agent_update()` — the one emitter

[`src/magician_v2/agents/agent_update_emitter.rs`](../../../magician/src/magician_v2/agents/agent_update_emitter.rs).
Every `AgentUpdate` goes through it: builds an `AgentEventEnvelope` with
`event_type = "agent.update"` and calls `broadcaster.emit_agent_transport_event()`.
Serialization failures are logged and dropped (never panic).

### 3. `agent_update_journal` — durable log

[`src/magician_v2/agents/agent_update_journal.rs`](../../../magician/src/magician_v2/agents/agent_update_journal.rs).
`spawn_journal(config, broadcaster)` subscribes before returning (no race with
immediate emission), filters `event_type == "agent.update"`, and appends one
JSON line per event to `<data_root>/scopes/<principal>/<workspace>/updates.jsonl`,
rotating past `max_file_bytes` (default 50 MB). Each write is a single
`write_all` of line+newline under `O_APPEND`, so a concurrent reader never sees
a half-line.

`tail(...)` reads the current file only (rotated files are archival), filters
by `since_ts`, sorts newest-first, truncates to `limit`. `tail_matching(...)`
applies `agent` / `kind` / `thread` predicates before truncation so callers get
`limit` matching events. Both are fully synchronous whole-file reads and
parses, so callers on an async runtime must use `spawn_blocking` (the REST
handler does).

### 4. `feed_stall_watchdog` — silent-failure detection

[`src/magician_v2/agents/feed_stall_watchdog.rs`](../../../magician/src/magician_v2/agents/feed_stall_watchdog.rs).
Tracks cycles from `cycle_started` plus any later scoped event. A cycle silent
longer than `stall_after` (default 10 min) gets one `FeedStalled` with
`silent_for_ms` (`stall_emitted` prevents re-emit). Terminal events
(`cycle_completed/failed/paused`, `goal_failed`) clear tracking; `gc_after`
(default 24 h) evicts stale entries so the map is bounded. The watchdog ignores
`feed_stalled` in its own extraction, so it never re-triggers itself.

### 5. REST endpoint

`GET /api/magician/v2/updates` — [`agent_updates_api.rs`](../../../magician-api/src/agent_updates_api.rs).

| Param | Type | Default | Required | Description |
|---|---|---|---|---|
| `workspace` | string | — | no | Ignored compatibility field; the verified bearer scope is authoritative |
| `since` | i64 | — | no | Return events with `ts > since` (unix ms) |
| `limit` | usize | 200 | no | Max events (hard max 1000) |
| `agent` | string | — | no | Filter by `agent_id` |
| `kind` | string | — | no | Filter by variant tag (e.g. `cycle_completed`) |
| `thread` | string | — | no | Filter by `thread_id` |

Scope (principal / workspace) comes from the verified bearer
(`resolve_required_scope`); both are validated by `is_unsafe_scope_id()`
(rejects empty, `.`, `..`, leading dot, `/`, `\`, NUL, control chars) → `400`.
Response: `{ "count": N, "events": [AgentUpdate…] }`, newest-first, `count`
after filtering.

## Emission sites

### A. Inline emission (preferred — rich context at source)

| Site | File | AgentUpdateKind |
|---|---|---|
| `emit_agent_cycle_started` / `_completed` | `src/magician_v2/agents/events.rs` | `CycleStarted`, `CycleCompleted` |
| `emit_approval_requested_event` / `_resolved_event` / `_expired_event` | `magician-api/src/web_api.rs` | `ApprovalRequested`, `ApprovalResolved`, `ApprovalExpired` |
| Tool-output-file creation + persist-failure | `src/magician_v2/execution/agentic/executor.rs` | `ArtifactCreated`, `ArtifactCreateFailed` |
| `emit_task_updated_transport` (maps `task.state.status`) | `src/magician_v2/artifact_v2/service.rs` | `TaskCreated`/`Updated`/`Completed`/`Failed` |
| `consolidate_cycle_completed_v3` call site | `src/magician_v2/artifact_v2/service.rs` | `TierConsolidated` (one per target) |
| `record_delegation_launch_dispatched` | `src/magician_v2/artifact_v2/service.rs` | `DelegationIssued` |
| `reduce_child_execution_terminal{,_without_output}` | `src/magician_v2/artifact_v2/service.rs` | `DelegationResolved` |

### B. Translator (legacy derived events — single chokepoint)

`agent_update::agent_update_from_legacy_event(event_type, payload, agent_id, workspace_id)`,
called from `emit_derived_agent_events` in `web_api.rs`, maps:
`agent.goal.{completed,failed,recovered}`, `agent.circuit.{opened,recovered}`,
`agent.{created,updated,deleted,paused,resumed}`, `agent.memory.report`,
`agent.feedback.generated`, `published_surface.changed`. It skips
`agent.cycle.*` and `approval.*` (emitted inline).

#### Synthesized `agent.circuit.recovered`

The runtime has no "circuit closed" wire event; recovery is implicit in
`outcome_transition.recovered`. A derived `agent.circuit.recovered` is
synthesized at the `agent.goal.recovered` push site, gated on
`previous_failures >= definition.constraints.max_consecutive_failures`
(default 3), so single-failure blips that never tripped the breaker do not
produce a card.

## Startup wiring

[`magician-bin/src/main.rs`](../../../magician-bin/src/main.rs) spawns
`spawn_journal(AgentUpdateJournalConfig::with_workspace_layout(storage_workspace), broadcaster)` and
`spawn_watchdog(FeedStallWatchdogConfig::default(), broadcaster)` right after
`MuijDeltaEmitter::spawn_scoped`. Handles are intentionally leaked (process
lifetime). `AgentUpdateJournalConfig` is also registered as `web::Data` so the
REST handler reads the same config.

## Design decisions

- **Single event topic, discriminated internally.** Reuses the existing
  `agent.ui.delta` topic pattern; the envelope's `event_type` is already the
  discriminator.
- **No `InsightGenerated` variant.** Insights are redundant with the meta
  harness, circuit breakers, consolidation rules and Attention Bar.
- **`DelegationResolved` uses execution ids in `from_agent`/`to_agent`**
  (`parent:exec_xxx`, `child:exec_yyy`) because the reducer layer does not
  expose parent-agent identity.
- **`TaskUpdated` maps all 7 task statuses**; consecutive same-status
  emissions produce duplicates, which the UI collapses by kind.
- **Path-traversal validation on the read side only.** Envelope sources are
  internal and trusted.

## Operational notes

- Rotated journal files are not garbage-collected.
- `/updates` reads the full current JSONL each call (bounded by the 50 MB rotation).
- Cycle `duration_ms` is wall-clock (`std::time::Instant`) around `execute_agentically()`.
- Tests live beside each module (`agent_update`, `agent_update_emitter`,
  `agent_update_journal`, `feed_stall_watchdog`, `agent_updates_api`).
