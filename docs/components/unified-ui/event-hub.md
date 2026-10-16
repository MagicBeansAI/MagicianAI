# Event hub (web)

`src/lib/realtime/eventHub.ts` owns the web app's **one** realtime connection,
the `/api/magician/v2/realtime/ws` WebSocket. Features subscribe to it; none
opens a socket, a live `/v3/events` tail or a chat-turn `/events/stream` of
its own. History is REST: one request per mount, and one more per resync.

Contract: `docs/archive/plans/2026-10-01-single-event-hub.md`.

## Why

Every live HTTP stream holds one of the browser's ~6 connections per host.
Per-feature streams (task cards, the HITL badge, the notify overlay, chat turns)
would take all of them on a busy `/chat`, queuing API calls and the page itself
behind them.

## Subscribing

```ts
import { subscribeEvents, subscribeWithBackfill } from '$lib/realtime/eventHub';

const off = subscribeEvents(
	{ categories: ['hitl'], userRelevant: true },
	(event) => apply(event.raw),
	(reason) => void refetch() // the hub may have missed events
);
```

- `subscribe(filter | predicate, onEvent, onResync?) → unsubscribe`. The
  first subscriber opens the socket.
- Filter fields: `categories`, `eventTypes` (prefix match on the outer
  `event_type` **and** on the `AgentEvent` inner type), `taskId`,
  `executionId`, `chatTurnId`, `agentId`, `sessionId` (each a string or a
  list), `userRelevant`.
- Every delivered `HubEvent` carries `raw` (the frame), `eventType` (outer),
  `type` (inner for `AgentEvent`, else outer), `inner` (the unwrapped
  `data.event` envelope), `payload` (`inner.payload`, else `data`), `seq`,
  `epoch`, `categories`, `userRelevant`, `refs` and `ref(key)`. Subscribers
  never unwrap `AgentEvent` by hand.
- `subscribeWithBackfill({ filter, backfill, onEvent })` runs `backfill` (one
  REST request that applies history) at subscribe time and on every resync.
  Live events that arrive meanwhile are held and applied after it settles,
  so history never lands on top of newer live state.
- `v3EventsPredicate({ categories, severities, userRelevant, eventType,
  agentId, search })` is the `/v3/events` live-tail filter as a predicate, for
  surfaces that used to pass those as query params. Like the server's tail it
  does not filter by task or execution — those params only bounded backfill.
- `canonicalEventLine(frame)` serializes a live frame as the `/v3/events`
  NDJSON line, so surfaces that dedupe backfill against live by line keep
  working.

## Fallbacks for a server without the hub fields

The hub works against a server that sends no `seq`, `epoch`, `categories`,
`user_relevant`, `refs` or control frames:

- **Categories / user relevance.** Absent `categories` (or an empty list):
  the unwrapped type is looked up in the generated `event-taxonomy.ts`, as
  `events_api::serialize_event_if_passes` does. An unclassified type
  soft-passes category and `userRelevant` filters instead of being dropped.
- **Refs.** Each absent ref is read from the payload: the frame, `data`,
  `data.item`, `data.event`, `data.event.payload`, `data.message`,
  `data.message.metadata`, `data.payload`, `payload`. `agent_id` prefers the
  envelope's own; `session_id` also reads `chat_session_id`; `chat_turn_id`
  falls back to a bounded deep scan (the turn sink's `find_string_field`).
  An id filter with no matching id rejects the event.
- **Resume.** No `hub.hello` means no resume position: every reconnect
  resyncs. The hub waits 2 s for a hello after a reconnect, or decides at the
  first non-hello frame.

## Connection behaviour

- **Reconnect forever.** Jittered exponential backoff from 1 s, capped at
  30 s, with no attempt limit. When the page becomes visible, the window
  regains focus or the browser comes back online, a closed socket is redialed
  at once.
- **Resume.** After a `hub.hello`, each dial carries
  `resume_epoch=<epoch>&resume_seq=<last seq>`. `hub.hello.resumed=true`
  means the gap was replayed: no resync. `resumed=false` on a reconnect
  resyncs every subscriber once (the server's following `hub.resync` is
  absorbed). A new scope (principal/workspace) drops the resume position,
  reconnects and resyncs.
- **Dedupe** by `(epoch, seq)` over the last 8192 frames. Frames without
  `seq` are never deduped.
- **`hub.resync`** (`gap` / `lagged` / `epoch`) fans out to every
  subscriber's `onResync`; the socket stays open.
- **Replay target.** `v2Events.connect(executionId, agentId)` no longer
  re-targets the socket. It names the execution/agent whose pending
  `WaitingForUser` events the server re-emits on connect; the hint rides the
  next dial (cold start or reconnect), so the shared socket is never closed
  under other features. `/debug` clears the hint instead of
  closing the socket on reset and unmount.
- App ping `{"type":"ping"}` every 30 s, plus a zombie check that redials a
  socket no longer `OPEN`.

## `v2Events`

`v2-websocket.ts` (`v2Events`) is a frame listener on the hub. It keeps the
last-100-events store and routes frames into the app's stores (chat, MUIJ,
agents, approvals, media, theme). Its `connect`/`connectGlobal` ensure the
hub's socket; `connectionStatus`, `isConnected`, `send` and
`getConnectionState` delegate to the hub. `disconnect()` closes the app's
only socket and is for whole-app teardown (HMR, tests) — never for a page
unmount.

Only event types on its allow-list reach the stores. `TaskDeleted` (archive
and delete) is on it, and `taskStore` refreshes on it as it does on
`TaskUpdated`, so a deleted task leaves every list at once.

## One hub per webview

Each JS context gets its own hub. The notify overlay
(`/notify-overlay`, its own Tauri webview) imports `eventHub.ts` without
`v2-websocket.ts`, so it opens one socket for itself and routes nothing into
the main app's stores.

## Consumers

| Consumer | Live filter | History (REST, per mount + resync) |
|---|---|---|
| `stores/pendingHitlStore.ts` | `categories: ['hitl'], userRelevant: true` | `/v3/events?category=hitl&user_relevant=true&backfill_only=true` (map reset first) |
| `notify/notifyStream.ts` | `hitl,pipeline,execution,agentic`, user-relevant | same, `backfill_only=true` (cards reset first) |
| `components/ChatTurnProgress.svelte` | every event; `eventBelongsToTask` keeps a task card to its task | `/v3/events?execution_id&task_id&backfill_only=true` (only with an execution id) |
| `stores/chatTurnEventsStore.ts` | events with the turn's `chat_turn_id`, or with a `task_id`/`execution_id` that an event of the turn named | `/v2/chat/sessions/{sid}/turns/{cid}/events` |
| `stores/codingSpineStore.ts` | type contains `coding.` | `/v3/events?event_type=coding.&backfill_only=true` (24 h, 600) |
| `shell/vibe/VibeLegacyStudio.svelte` | type contains `coding.` | same (6 h, 80; rows rebuilt) |
| `today/TodayRealtimeWire.svelte` | every event | `/v3/events?limit=20&backfill_only=true` |
| `realtime/EventStreamCard.svelte` | `v3EventsPredicate` of its filters | `/v3/events?…&backfill_only=true`, re-read on filter change |
| `routes/(app)/runtime/ActivityView.svelte` | `category=activity` | `/v3/events?category=activity&backfill_only=true` |
| `routes/warroom/+page.svelte` | every event | `/v3/events?limit=80&backfill_only=true`; resync reads `since` the newest row |
| `shell/InteractiveTerminalPane.svelte` | `InteractivePtyChunk` for its `session_id` | `/interactive-sessions/{id}/buffer` (xterm reset first); also re-read on an offset gap, offsets restarting at 0, or after a failed read |

The chat-turn store learns the task/execution → turn mapping from live
events that carry both a `chat_turn_id` and a task/execution id, and from the
turn's REST projection (every row of which the server's turn sink routed to
that turn). That is the client side of the sink's fan-out recovery for
inner-loop events that carry no turn id. The 30 s / 60 s REST watchdog
remains as the backstop.

## Tests

`src/lib/realtime/eventHub.test.ts` covers filter matching (taxonomy
fallback, soft-pass, server fields, ref fallback), `AgentEvent` unwrap,
`(epoch, seq)` dedupe, resume parameters, resync fan-out, the pre-hub
fallback, unlimited reconnect within 30 s, the foreground retry, the replay
target and `subscribeWithBackfill` ordering. Consumer tests:
`stores/chatTurnEventsStore.test.ts`,
`stores/pendingHitlStore.hub.component.test.ts`,
`components/ChatTurnProgress.component.test.ts`.
