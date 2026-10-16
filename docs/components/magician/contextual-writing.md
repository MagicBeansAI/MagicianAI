# Contextual Writing API

The backend behind every contextual-assist surface: the desktop's left-Option
assist menu, the Chrome right-click flow (extension → desktop host gateway →
this API), the macOS/iOS Action extension, and the Magican Keyboard's Write
lane. It runs one contextual-writing action (rewrite / draft_reply /
continue_draft / summarize_page / …) over a supplied `context.contextText`
plus optional visual context, on a per-scope contextual-writing chat session,
and returns the produced draft — and, for task actions, a created task.

Source: `magician-api/src/contextual_writing_api.rs` (routes mounted in
`magician-bin/src/main.rs`).

## Endpoints

- `POST /api/magician/v2/contextual-writing/actions` — run one action.
- `POST /api/magician/v2/contextual-writing/sessions` — pre-allocate a
  durable titled session before a long turn (retry/cancel cannot mint
  duplicates).
- `GET /api/magician/v2/contextual-writing/catalog` — the canonical
  states→actions catalog. The desktop currently serves its own copy to its
  menu; this endpoint is the contract copy every other client should consume
  instead of hardcoding actions, and is the migration target for the desktop
  itself.

## Request (camelCase)

- Scope is not part of this body. The desktop sends its workspace-bound bearer,
  and the server derives principal/workspace from that credential.
- `action` — `{ id, label, intent, requiresScreenshot, createsTask, opensHud }`.
  `opensHud: true` is rejected server-side (400): HUD handoff is a client-only
  concern.
- `context` — `{ state, personality, app?, windowTitle?, url?, frameUrl?,
  contextText?, hasContextText }`. `frameUrl` carries the specific iframe URL
  when the target lives inside a frame distinct from the top-level page.
  `state` is validated against a closed vocabulary: `selection`,
  `selection-field`, `draft`, `empty-context`, `empty-no-context`,
  `page-context`, `files`, `secure`, `excluded`, `unsupported`,
  `thinking_map` (Thinking Map frontier only).
- `routing` — `{ agentId, sourceKind, sourceKey, sessionKey, rootUrl?,
  targetTextKind, actionIntent, personality }` plus typed feature fields
  (`surface`, `featureMode`, `threadId`, `sessionId`, `sessionTitle`).
  `targetTextKind` is validated against: `selected_text`, `field_text`,
  `screen_context`, `page_url`, `file_paths`, `none`, `thinking_graph`.
  `sessionKey` keys the reused contextual-writing session.
- `userPrompt?` — free-form guidance for the action.
- `visualContext?` / `screenshot?` / `reuseScreenshotAttachmentId?` — for
  screenshot-backed actions.
- **`chatTurnId?`** — optional client-supplied turn id (exact parity with
  chat's `chat_turn_id`). When present it is used for the underlying chat
  turn instead of a server-minted id; when absent one is minted. This lets a
  client that knows `(sessionId, chatTurnId)` tail
  `GET /chat/sessions/{sessionId}/turns/{chatTurnId}/events/stream` —
  **NDJSON lifecycle/tool/progress events, no draft text** — for honest stage
  labels during the otherwise-synchronous wait.

## Task actions

Actions flagged `createsTask` (Task, Follow-up) are honest: the prompt asks
the model to draft a concise task proposal whose first line becomes the
title, and after a successful draft the backend creates a real V3 task via
`ArtifactV2Service` — in the same contextual thread, full draft as the
description, `chat_session_id` linked back to the producing session,
`created_by: contextual-assist`, approved. The response carries `taskId`.
If task creation fails the request still succeeds (the draft is returned,
`taskId` omitted, error logged) — the draft remains manually saveable.

Creation is not idempotent: each successful POST creates a fresh task
(uuid-minted, no idempotency key in `CreateTaskInput`), so a client retry
or a second click duplicates the task. Clients that retry should treat
`taskId` in hand as success and not re-send.

## Response (camelCase)

`{ status, sessionId, threadId, chatTurnId, sessionTitle?, draftText?,
taskId?, userMessageId?, assistantMessageId?, attachmentIds,
screenshotAttachmentId?, provenance, queued? }`.

- `status` — `draft_ready` | `queued` | `accepted`.
- `draftText` — the produced draft (absent when queued).
- `taskId` — present only when a `createsTask` action produced a draft and
  the task was durably created.
- **`chatTurnId`** — echo of the (client-supplied or minted) turn id.
- `sessionId` / `threadId` — the contextual-writing session/thread the turn
  ran on.

## Notes

- The endpoint responds only after the turn completes (no draft-text
  streaming on this lane; token streaming lives on the chat
  `messages/stream` lane). Stage labels come from the separate turn-events
  NDJSON feed, keyed by `(sessionId, chatTurnId)`.
- `app`, `windowTitle`, `url`, `frameUrl`, and `rootUrl` are prompt grounding
  and session-title inputs; routing-by-source (`site:{rootUrl}` vs
  `app:{name}` session keys) is client-owned and arrives via
  `routing.sourceKey`.
- A text-less action is valid only when a screenshot is required
  (`visualContext.screenshot.required`).
- Thinking Map (Brainstorm) is the one typed feature surface: exact
  featureMode/surface request contracts authorize the
  `brainstorm-facilitator` agent; Tutor and App Copilot are refused here and
  require their dedicated authenticated product routes.
