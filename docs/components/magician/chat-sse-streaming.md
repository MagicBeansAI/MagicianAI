# Chat SSE Streaming

## Purpose

Chat SSE streaming lets `/chat` render assistant output incrementally
instead of waiting for the full response body.

## Transport

The backend exposes `POST /api/magician/v2/chat/sessions/{id}/messages/stream`
as a `text/event-stream` endpoint (`Cache-Control: no-cache`,
`X-Accel-Buffering: no`). The client consumes it with `fetch` plus
`ReadableStream`.

While idle, the response sends an SSE comment (`: keep-alive`) every fifteen
seconds so intermediaries see traffic during long tool or human-response waits.
Comments are transport traffic, not chat events. Completion closes the stream.
Disconnect handling is under [Disconnect cancel](#disconnect-cancel).

`validate_profile_override` runs before the stream opens. An unknown
`profile` on a non-public surface is `400` (`Profile '{name}' is not a
valid chat profile`). Public-chat source surfaces defer to the service
fallback instead of rejecting.

A non-streamable turn still uses this route and returns a single `done`
event: `mode: "plan"` runs the synchronous plan path and emits one `done` with
the persisted `ChatResponse`, and a profile without streaming (see
[Profile Gating](#profile-gating)) streams no token frames: the router sends
only a provider `Done`, which is suppressed, so the client gets the terminal
`done`.

## Event vocabulary

Each frame is `event: <type>` plus a JSON `data` payload. Provider-level
`StreamDelta::Done` is suppressed; after the channel closes the handler
appends its own `done` carrying the persisted `ChatResponse` (or `{}` if
the slot is empty). The formatter maps these `StreamDelta` variants:

- `token` — `{"text": "<delta>"}`
- `tool_call` — `{"id", "name", "arguments_chunk", "status": "executing"}`
- `tool_call_start` — `{"call_id", "tool_name"}`
- `tool_call_args_delta` — `{"call_id", "delta"}` (concatenate `delta` by `call_id`)
- `tool_call_end` — `{"call_id"}`
- `reasoning_start` — `{"index", "signature"}`
- `reasoning_delta` — `{"index", "delta"}`
- `reasoning_end` — `{"index", "total_chars"}`
- `error` — `{"error": "<message>"}`
- `done` — persisted `ChatResponse` (`user_message`, `assistant_message`,
  `messages[]`, `session_title`, `queued`, `cancelled`,
  `pending_queue_depth`, `usage`, …)

## Runtime Flow

1. The chat service persists the user message first (`persist_user_turn`,
   including `chat_turn_id` on the streaming path).
2. For authenticated agent lanes, the service resolves the scoped agent
   definition and trust policy before constructing the provider catalog. A
   missing definition or policy fails closed as an SSE error; tests use a real
   writable scoped store rather than treating the read-only template directory
   as runtime state.
3. The LLM provider streams token deltas through an internal channel.
4. The chat service drains that internal channel concurrently while the provider
   is still producing deltas; it does not wait for provider completion first.
5. The API forwards arriving deltas as SSE events (see above).
6. Provider-level completion is suppressed.
7. After persistence and any synchronous tool continuation, the service emits a
   final `done` with the persisted chat response, so the terminal event matches
   the stored assistant message rather than the raw provider completion.

## Tool Interaction

Streaming covers assistant text generation. Tool execution still happens
synchronously between streamed turns. If a tool loop runs, the final `done`
event reflects the post-tool continuation result. Live tool and reasoning
progress also land on the per-turn
`GET .../turns/{cid}/events[/stream]` tail.

Large tool results do not replay on the SSE stream. Display continuation
uses authenticated `POST /api/magician/v2/chat/sessions/{id}/results/read`
(opaque locator; every page rechecks scope, owner, agent, tool/trust
policy, authority revision, retention, cursor, and content hash). See
[chat-mode.md](chat-mode.md#unified-tool-result-and-turn-context-contract).

## Profile Gating

Streaming is profile-gated. Profiles without `metadata.streaming: true` fall
back to a synchronous `invoke` inside the magicllm router's stream path
(`magicllm/src/router.rs`), which sends the whole response as one
`StreamDelta::Done`.

## Disconnect cancel

`SseDisconnectCancelGuard` wraps the response stream. When the client drops
the connection (`AbortController.abort()`, tab close, HTTP/2 `RST_STREAM`),
`Drop` normally calls `cancel_chat_run` on the session and cancels the scoped
tutor run, racing the in-flight turn against `cancel_token.cancelled()`.

An authenticated client that sends `continue_on_disconnect: true` makes that
guard inert, so mobile process or radio loss is not an implicit Stop: the
detached worker finishes and persists the accepted turn, and the client
recovers it from the canonical transcript/realtime feed. Opted-in
`mode: "plan"` work follows the same rule through a detached task and still
emits only its terminal frame to a connected caller. If the watched turn
already wrote `chat_response_slot`, Drop also noops so a pending-queue drain
that claimed the same active-run slot is not cancelled. Explicit Stop remains
`DELETE /chat/sessions/{id}/run`.

## Frontend Behavior

`chatStore.sendMessageStreaming` inserts a temporary user message and an
assistant placeholder, appends streamed `token.text` into that bubble, then
replaces both with the persisted server messages when `done` arrives. The
store currently consumes `token`, `done`, and `error`; other event types are
ignored on this path.

Attachments do not stream: if `attachmentIds.length > 0`, the store calls
`sendMessage` (`POST .../messages`) up front. The composer does the same
before invoking the streaming helper.

On any streaming failure the store does not re-send. It reconciles via
`fetchSessionDetail`, drops the placeholder, and attaches a synthetic
assistant error bubble.
