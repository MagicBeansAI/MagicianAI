# Chat turn progress card

`src/lib/magician/components/ChatTurnProgress.svelte` renders the live
"Working…" bubble under an in-flight chat turn: an ordered list of
step/tool/iteration rows that stream in as the turn runs.

## Row model

Each event, live from the app's event hub ([event-hub.md](event-hub.md)) or from
the card's one `/api/magician/v3/events` backfill request, folds into a keyed row
(`rowsByKey`) with a status:

- `running`: the start event arrived; renders a spinner.
- `done` / `failed`: a matching completion event settled it.
- `waiting`: paused (e.g. HITL). A `pause:<correlation_id>` row settles to
  `done` ("Answered") on `hitl.resolved` / `input.received` / `input.resolved`
  with the same correlation id (an id-less `pause:pause` row settles with any
  answer, and an answer without an id settles every waiting pause row). The
  "Waiting on you" head clears when no pause row is left waiting.

Rows coalesce by a stable key (`step:<step_id>`, `iter:<n>`, tool keys,
`tutor:<run>:…`), so updates land in place.

## Next variant

`variant="next"` (default `'classic'`) is presentation only: the same rows render
through the chat-next `TurnStage` (`src/lib/magician/chat/next/`). The running
row becomes a working chip, settled rows collapse into status dots, and a toggle
expands the full list. It renders every row, not the `visibleRows` tail. Each row
maps to `{ key, label, status }`, plus:

- `tool`: the tool name, for `tool:<call_id>` rows only.
- `target`: the host of `args.url` / `args.href`, or the basename of
  `args.path` / `file_path` / `filename` / `file`; for artifact rows, the file
  name. For an MCP `<pack>__call_tool` call, a ≤80-char summary of
  `args.arguments_json` (its `sql` / `query` / `term`-like field, else the
  compact JSON).

Tool row names come from `describeToolCall` (`stageRows.ts`): a server `detail`
(e.g. `metabase-mcp · execute_sql — SELECT …`, ≤100 chars) wins; a
`<pack>__call_tool` call reads `<pack> · <remote tool>`; otherwise the tool name.
A finish without args keeps the name its start gave the row.

**Delegated/agentic steps.** A delegated run's tool steps arrive as
`AgenticActionStarted` and `AgenticActionExecuted`, not `tool.call.*`.
`agenticActionAsToolEvent` (`stageRows.ts`) maps them onto `tool.call.started` /
`.finished` / `.failed` (tool name `target ?? action_type`) in both
`ChatTurnProgress` and `RequestActivityCard`, keyed by the server's shared
`call_id` so the finish settles the start's row. A legacy executed event without
`call_id` keys on `step_id-iteration-timestamp` and, in `ChatTurnProgress`, opens
its own settled row.

`tool` and `target` come from a side map (`toolMetaByKey`) filled in the tool
branch of `handleEvent`; it never feeds row status or settling. The row → stage
mapping (`toStageRows`, `toolTarget`) lives in
`src/lib/magician/chat/next/stageRows.ts`, shared with `RequestActivityCard`.
With nothing running, the stage chip is a `--radius-chip` label: `Waiting on your
input` when a row is `waiting`, else `Finishing…`. The `.chat-turn-progress`
wrapper and the "Working… / Waiting on you" head render in both variants.
Covered by `ChatTurnProgress.component.test.ts` and
`TurnStage.component.test.ts`.

## Bare

`bare` (default `false`) is presentation only, for hosts that already frame the
progress (the chat-next live task card). It drops the "Assistant" header, the
`chat` / `chat-bubble` surface and the "Working…" head. A HITL pause still shows
as a compact accent `Waiting on you` line (`.chat-turn-progress__pause`,
`role="status"`).

## Terminal settling

A per-row completion event cannot be relied on to clear a row:
`agentic.iteration_started` creates an `iter:` row but the backend emits **no**
`agentic.iteration_completed`, and a step/tool completion can be **dropped** (the
live feed is best-effort) or arrive with a **key that doesn't match** its start.

So a **terminal catch-all** (`closeAllRunningRows`) settles every still-`running`
row when the turn or task ends. It keys off:

- `task.status_changed` with `completed` / `failed` / `cancelled`;
- the **top-level** execution terminal (`execution.completed` / `.failed` /
  `.cancelled`, `agentic.execution_completed`), scoped by `execution_id`, so a
  sub-agent's completion cannot close the parent's rows.

## Surviving refresh and reconnect

The card holds **no stream of its own**. It subscribes to the app's event hub
(`subscribeWithBackfill` in `src/lib/realtime/eventHub.ts`); every event in the
scope is offered, and a task-scoped card keeps only its task's events via
`eventBelongsToTask`. This keeps a busy `/chat` within the browser's ~6
connections per host.

- **Backfill on mount.** Scoped to a unique `execution_id`, the card makes
  **one** `GET /api/magician/v3/events?execution_id=…&task_id=…&backfill_only=true`
  request, so a page refresh restores and settles the timeline (the replayed
  terminal fires the catch-all). Live events arriving meanwhile are held and
  applied after it. Without an `execution_id` the card is live-only, since an
  unbounded backfill could leak a prior run's events.
- **Resync.** The hub reconnects forever. When a reconnect cannot resume (a
  pre-hub server, an expired resume window) or the server reports lag, the hub
  resyncs and the card repeats the backfill, so a terminal fired during the gap
  is recovered.
- **Idempotent replay.** `handleEvent` dedups via the shared `eventDedupeKey`
  (exported from `chatTurnEventsStore`); `rowsByKey` persists across resyncs, so
  rows are never double-counted.

## The reply's step strip (`RequestActivityCard`)

- **Model label.** An LLM row reads "Thinking with <model>". `llm.requested`
  names the requested model (for an external harness, the engine label such as
  "Claude Code · opus", with `engine` / `harness_model`). When `llm.succeeded` /
  `llm.failed` / `LLMResponseReceived` names a model (`model`, else
  `harness_model`, else `provider`), the row takes the answering model
  (`llmModelLabel.ts`); a settled event naming none keeps the running label.
  `ChatTurnProgress` relabels a finished call "Response ready", and the task
  timeline keeps the capability title with the answering model on its meta line.
- **Delegated task rows.** A `task::<id>` row settles on a terminal
  `chat.delegate.status_changed` / `task.status_changed` /
  `chat.delegate.output_ready`. When `live` flips false while such a row still
  runs, the card keeps its turn subscription until the row settles and re-reads
  the turn once. Cards that were never live open no tail. ChatPanel also passes
  `transcriptTaskStatuses` (task_id → latest transcript card status); a running
  task row whose latest card is terminal settles from it (`taskRowSettle.ts`),
  and the cleanup pass then settles its child rows.

## Contrast: the deep execution panel

The unified task panel does **not** derive step status from a live event stream.
It fetches full state from `/api/magician/v3/executions/{id}/execution-panel`,
and `executionPanelStream.ts` replaces it with each `ExecutionPanelDelta` full
snapshot. Every render reflects the backend's settled statuses, so it is
refresh- and reconnect-safe and needs no catch-all.

`RequestActivityCard` settles once the host drops `live` (the reply landed, or the
spawned task ended): every still-`running` step settles to `done` and a task row
to `idle`, unless a row is `waiting` on the reader. A non-live card has no live
tail, so a running row there could only be one whose terminal event was lost.
The live task card's run list (`toLiveTaskView`) likewise settles a run still
marked running once the task is terminal.

## Which turn stays live: `liveTurn.ts`

A delegate task spawned by a chat turn keeps emitting events after that turn
finished. Those events fan to the spawning turn's id, so the card must stay
subscribed past the turn's own completion.

`latestActiveTaskTurnId` (`$lib/magician/chat/liveTurn`) answers **which turn
still needs a live tail**, given the session's visible messages. Two rules:

1. **Reduce to the latest row per task first.** Task progress is append-only, so
   only the newest row per `task_id` counts; an old `running` row for a task that
   has since completed keeps nothing alive.
2. **Require explicit task-local turn correlation.** A row carrying a
   `chat_turn_id` establishes the spawning turn for that task; later rows for the
   same `task_id` may reuse it when a producer omitted the field. An entirely
   uncorrelated legacy task card is never attached to the session's newest user
   turn (that would leave a permanent Steps spinner).

A task whose latest chat row is non-terminal but whose task-store status is
terminal for the same execution (`storeStatusForCard`; a terminal status of an
earlier run is ignored) is over too: `ChatPanel` passes that as
`isSettledElsewhere`, and the live task card takes the store's terminal status.

Terminal statuses are `completed`, `failed`, `cancelled`. The resolver is
injectable (`ChatTurnIdResolver`) for callers whose correlation lives in a
side-map; the default reads `message.chat_turn_id`.

## Opening a complete tool result

Activity navigation and result authorization have different owners. A delegation
row can carry `task_id`/`execution_id` so **Inspect run** opens the child task.
The card takes the newest event carrying both ids as the run, and combines ids
from separate events only when no event pairs them (inline calls after a
delegation stamp the chat-turn id as their `execution_id`). The initiating
`delegate_to_agent` result is still owned by the parent Chat session.

`tool.result.projected.result_owner` is authoritative for **Open complete
result**: Chat owners use the scoped Chat read API, task owners the
task/execution read API, and ephemeral voice owners never fall back to an
unrelated session. The task-id heuristic remains only for durable events written
without owner metadata. Web, iOS and Android read the versioned pages to
completion, verify stable identity/hash/order/selection, and rebuild complete
values, typed containers and UTF-8 fragments before showing the result.

Covered by `liveTurn.test.ts` across stale, terminal-superseded,
correlated-active, omitted-correlation continuation, omitted-correlation
terminal, and side-map-correlated histories.

The backend half is in `magician`: chat progress subscriptions copy their
spawning `chat_turn_id` onto synthesized task-status and delegate progress
messages, immediate delegation status cards keep the same id, and
external/realtime task attachment keeps the originating turn rather than
replacing it with an uncorrelated stream.
