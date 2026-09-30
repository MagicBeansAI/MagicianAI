# Chat turn progress card

`src/lib/magician/components/ChatTurnProgress.svelte` renders the live
"Working…" bubble under an in-flight chat turn — a compact, ordered list of
step/tool/iteration rows that stream in as the turn executes.

## Row model

Each event from the turn's `/api/magician/v3/events` stream is folded into a
keyed row (`rowsByKey`) with a status:

- `running` — the row's start event arrived; renders a spinner.
- `done` / `failed` — a matching completion event settled it.
- `waiting` — paused (e.g. HITL).

Rows are coalesced by a stable key (`step:<step_id>`, `iter:<n>`, tool keys,
`tutor:<run>:…`) so updates land in place without reshuffling.

## Terminal settling

A matching per-row completion event cannot be relied on to clear a row:

- `agentic.iteration_started` creates an `iter:` row, but the backend emits **no**
  `agentic.iteration_completed` — nothing could ever close it by key.
- A step/tool completion can be **dropped** mid-stream (SSE is best-effort) or
  arrive with a **key that doesn't match** its start event.

So a **terminal catch-all** (`closeAllRunningRows`) settles every still-`running`
row when the turn/task terminates. It keys off:

- `task.status_changed` with `completed` / `failed` / `cancelled` — turn/task
  level, unambiguously "all done".
- the **top-level** execution terminal (`execution.completed` / `.failed` /
  `.cancelled`, `agentic.execution_completed`), scoped by `execution_id` (a
  sub-agent's completion carries its own id, so it can't prematurely close the
  parent's rows).

## Surviving refresh and reconnect

The card owns its own event subscription (it does **not** go through
`chatTurnEventsStore`), so it also carries that store's resilience itself:

- **Backfill on mount.** When scoped to a unique `execution_id`, the subscription
  drops `since=now` so the stream **replays the per-turn projection first**. A
  page refresh therefore *restores and settles* the timeline (the replayed
  terminal fires the catch-all). Without an `execution_id` it falls back to
  `since=now` (a backfill can't be bounded safely without leaking a prior run's
  events).
- **Reconnect with backoff.** If the stream ends before the turn terminates
  (server close / timeout / mid-turn drop), it re-opens — which re-backfills — so
  a terminal that fired during the gap is recovered instead of spinning forever.
  It stops once a terminal is applied (`turnTerminal`) or on unmount.
- **Idempotent replay.** Both backfill and reconnect replay events already
  applied; `handleEvent` dedups via the shared `eventDedupeKey` (exported from
  `chatTurnEventsStore`), so rows are never double-counted or duplicated
  (`rowsByKey` persists across reconnect).

## Contrast: the deep execution panel

The unified task panel (which replaced the deep-work `ExecutionPanel`) does
**not** derive step status from a live event stream. The surface fetches the
full state from `/api/magician/v3/executions/{id}/execution-panel`, and
`executionPanelStream.ts` replaces it with each `ExecutionPanelDelta` full
snapshot. Because every render reflects the backend's authoritative,
already-settled statuses, it is inherently refresh- and reconnect-safe and needs
no equivalent catch-all. The standalone `ExecutionTimeline.svelte` (WS-driven)
carries a defensive `settleAllPending` on `ExecutionCompleted`/`Failed` for the
same reason as the chat card, but no surface mounts it.

## Which turn stays live: `liveTurn.ts`

A delegate task spawned by a chat turn keeps emitting transport events *after*
that turn finished — the assistant already replied "Handed to … agent" and
`isSendingMessage` went false. Those events fan to the spawning turn's id, so
the card has to stay subscribed past the turn's own completion, or steps only
reappear on a manual refresh.

`latestActiveTaskTurnId` (`$lib/magician/chat/liveTurn`) answers **which turn
still needs a live tail**, given the session's visible messages.

Two rules, and both matter:

1. **Reduce to the latest row per task first.** Task progress is an
   append-only history, so an old `running` row for a task that has since
   emitted `completed` must not keep anything alive. Only the newest row per
   `task_id` is considered.
2. **Require explicit task-local turn correlation.** A row carrying a
   `chat_turn_id` establishes the spawning turn for that task. Later status
   rows for the same `task_id` may reuse that established provenance when an
   older/cross-surface producer omitted the field. An entirely uncorrelated
   legacy task card is never attached to the session's newest user turn — that
   is what left completed chat and voice turns showing a permanent Steps
   spinner.

Terminal statuses are `completed`, `failed`, `cancelled`. The resolver is
injectable (`ChatTurnIdResolver`) so callers can supply a side-map when the
correlation lives outside the message itself; the default reads
`message.chat_turn_id`.

## Opening a complete tool result

Activity navigation and result authorization have different ownership. A
delegation row can carry `task_id`/`execution_id` so **Inspect run** opens the
child task, while the initiating `delegate_to_agent` result is still owned by
the parent Chat session. `tool.result.projected.result_owner` is authoritative
for **Open complete result**: Chat owners use the scoped Chat read API, task
owners use the task/execution read API, and ephemeral voice owners do not fall
back to an unrelated session. The task-id heuristic remains only for durable
events written without owner metadata. Web, iOS, and Android all read
the versioned pages to completion, verify stable identity/hash/order/selection,
and reconstruct complete values, typed containers, and UTF-8 fragments before
showing the result.

Covered by `liveTurn.test.ts` across stale, terminal-superseded,
correlated-active, omitted-correlation continuation, omitted-correlation
terminal, and side-map-correlated histories.

The backend half of this contract is in `magician`: chat progress subscriptions
copy their spawning `chat_turn_id` onto synthesized task-status and delegate
progress messages, immediate delegation status cards retain the same id, and
external/realtime task attachment preserves the originating turn instead of
silently replacing it with an uncorrelated synthetic stream.
