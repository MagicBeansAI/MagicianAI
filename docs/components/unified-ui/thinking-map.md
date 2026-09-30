# Live Thinking Map — web client (unified-ui)

`ui/unified-ui/src/lib/thinkingMaps/` + `ui/unified-ui/src/lib/types/thinkingMap.ts`.
The web/desktop surface for the canonical Live Thinking Map / brainstorm
(backend: [live-thinking-map](../magician/live-thinking-map.md)).

## Foundation

- **Types** (`lib/types/thinkingMap.ts`) mirror the backend wire
  (`thinking_map/{models,operations}.rs`; Swift `LTM.*` is the same contract):
  snake_case, enums as string-literal unions (11 node kinds, 7 epistemic states,
  6 assertion origins, 9 edge kinds, lifecycle/clarification/proposal states),
  internally-tagged unions as discriminated unions (`MapOperation` on `op` — 22
  variants; `OperationActor` on `actor`; `ThinkingMapSource` on `kind`;
  `ApplyOutcome` on `outcome`). `update_node.detail_markdown` is
  `string | null` (absent = leave, `null` = clear).
- **API client** (`lib/thinkingMaps/api.ts`) — scoped `timedFetch`
  (`scopedRequestHeaders` + `appendCurrentScopeQuery`), base
  `/api/magician/v2/thinking-maps`: create / list / get / patch / operations /
  interpret / consolidate / decideProposal / events / replay / restore /
  attach+detachSession / promoteNode / registerTutorContext. Non-2xx throws the
  body's `error`.

## Canvas + routes

- **`ThinkingMapCanvas.svelte`** — D3 force-directed canvas. Nodes are
  rounded-rect cards with a coloured kind header (11-kind → theme-token map);
  `model_inferred`/`provisional` render dashed + muted + ✦ (AI-suggested),
  `confirmed`/`resolved` emphasized, `contradicted`/`superseded`/`rejected`
  faded, tombstoned hidden; the selected node gets an animated accent ring. Two
  link types: semantic edges (arrowed, coloured/labelled by `EdgeKind`) and the
  `parent_id` tree (neutral dashed). Strokes are non-scaling, lines are clipped
  to card rectangles, and arrow tips use card-relative marker units, so zoom
  never shrinks or detaches arrows. Honors node `position`; all colours via
  `themeColor(token, fallback)` — D3 resolves tokens into SVG attributes, so
  theme changes re-render.
- **UX:** zoom-to-cursor (0.1–4), double-click zoom in (⌥/⇧ out), `+ − Fit`
  controls, immediate node drag that **persists** via `move_node` (owner),
  keyboard (`+`/`-`, `0`/`f` fit, arrows pan, `Esc` deselect), interruptible
  eased fit, zoom-% readout, edge labels on hover/selection only, spring-in new
  nodes (respecting reduced motion). Edge colours: green support, red
  contradiction, blue answers/measurement, amber alternatives/dependencies,
  secondary accent grouping, primary accent consequence, muted neutral loose
  relation — with dash rhythms so meaning does not rely on colour.
- **`routes/(app)/thinking-maps/+page.svelte`** — responsive table library with
  **Maps** and **Deleted** tabs, both via `listMapsPage` (`limit`/`offset`/exact
  `lifecycle`, most recent first), 25/50/100 rows, shared `ServerPager`. Stale
  overlapping responses are ignored; the last page clamps/refetches when
  deletion or restore empties it. Maps excludes tombstones before `total`;
  Deleted (`lifecycle=deleted`) offers **Restore** (PATCH to active) and **Delete
  permanently** (themed destructive confirm, server `confirm=permanent` gate,
  already-deleted maps only).
- **`routes/(app)/thinking-maps/[map_id]/+page.svelte`** — canvas plus a side
  inspector for the selected node (kind / epistemic_state / assertion_origin /
  confidence / detail), header title + revision, **✦ Continue** / **✦ Break
  open** (`interpret`) and **Reorganize** (`consolidate`).

## Brainstorm flow

`BrainstormComposer`, `ClarificationPanel`, `ProposalCard` and the page:

- **Capture** — owner `add_node` (client uuid, chosen kind,
  `owner_spoken`/`asserted`, `parent_id` = selected node so captures grow the
  active branch) → `applyOperations`.
- **AI structure** — Continue (`continue_thinking`) / Break open (`break_open`)
  add `model_inferred`/`provisional` nodes. Both send the selected node as
  request-scoped `focus_node_id` (so work continues from the visible branch even
  if another tab's shared active node is older); `no_operations`, invalid focus
  and `503 llm_unavailable` are surfaced, never a silent fallback to the root.
- **Clarifications** — answerable cards → `resolve_clarification`.
- **Restructure** — Reorganize stages a proposal (`ProposalCard`: rationale +
  "Affects N · M changes") → confirm/reject via `decideProposal`.
- **Live** — per-map `sharedPoll` over `getMap` (~4s idle, 1.2s fast; `pollNow`
  after each mutation); the latest polled `revision` is each op's
  `base_revision` (server-authoritative, no optimistic UI). A
  `ThinkingMapUpdated` WebSocket notice triggers `pollNow()` — push only
  accelerates polling (no payload), so a missed notice costs one interval, never
  correctness.
- **Interpret narration** — during Continue/Break open, a strip follows
  `ThinkingMapInterpretProgress` stages (`preparing` with live-thought count →
  `loading_context` → `facilitating` → `parsing` → `shaping`). The page mints an
  `utterance_id`, sends it on `/interpret`, and applies only stages tagged with
  it (`interpretProgress.ts`); unknown stages keep the line; terminal `idle` is
  ignored in favour of the HTTP response; socket down → "Exploring…". Navigating
  away clears it.

## Ambient "Listen" mode — voice → map

**🎙 Listen** starts a hands-free call
(`startVoiceCall({threadId: 'thinking-map-<id>', mode:'hands_free'})`) and
attaches the tab's media session id (`$mediaSessionStore.session.session_id`)
via `attachSession`. The server's ambient coordinator matches each finalized
user turn through the message's `presence_session_id` (backend
`resolve_binding`; the voice orchestrator's chat session id is server-derived and
never visible to the client) and auto-maps it; the fast poll surfaces nodes, with
`pollNow()` per finished turn. UI: status strip (pulsing dot, live caption from
`voiceTranscriptStore`, Stop) and a soft error banner. Start (media session →
`startVoiceCall` → connected) has one 25s timeout; `stopVoiceCall()` aborts a
pending start. Auto-stops (detach + hang up) on call drop, map switch and page
leave; detach is best-effort/idempotent.

## Attach a conversation — meetings/chats → map

**🗣 Attach conversation** picks from `GET /chat/sessions` (newest first,
filterable) and attaches that chat session id; its user turns and a meeting
bot's `meeting-transcript` System turns then auto-map through the coordinator.
Attached conversations show as pulsing chips with detach ✕. The page detaches
everything it attached on map switch and page leave — the server registry is
in-memory, so attachments are session-scoped by design.

## Promote — node → Task / Memory

**↗ Task** and **💾 Memory** in the inspector call
`promoteNode(id, nodeId, target, confirm?)` → `POST /{id}/nodes/{node_id}/promote`
(the governed promotion endpoint):

- Owner-asserted nodes promote in one click; a status strip shows the created id
  (~6s) and `pollNow()` refreshes.
- **409 `confirmation_required`** (AI-suggested / participant / imported) shows
  "This is AI-suggested — promote anyway?"; **Promote** retries with
  `confirm: true` (recorded server-side as an owner assertion).
- When `promoted_refs` already has that kind, the button becomes a **→ task / →
  memory** chip (idempotent endpoint).
- Other failures (`not_promotable`, `promotion_unavailable`, …) are a dismissible
  inline error.

## Ask Tutor about this node

**🎓 Ask Tutor** grounds the Personal Tutor in the selected node:

1. `activeChatSessionId('general')` →
   `GET /api/magician/v2/chat/active?ui_thread_id=general` (creates a session if
   none).
2. `registerTutorContext(mapId, sessionId, selectedNodeId)` →
   `POST /thinking-maps/{id}/tutor-context {session_id, node_id}` binds the
   node-neighbourhood digest to that session (server TTL ~30 min; re-clicking
   refreshes).
3. `goto('/chat')` — the next `@tutor` run receives the digest as background
   grounding (`TutorRun.thinking_map_context`); the tutor keeps narration and
   storyboard ownership.

Failures are a dismissible inspector error, cleared on node change.

## Time Loom — replay

A collapsible **⏪ Time Loom** panel (`TimeLoom.svelte` + pure `timeLoom.ts`)
scrubs the append-only event log:

- **Timeline** — loads the full log (`events(id, 0)`) on open and appends the
  tail as the live revision moves; the scrubber runs over event index (right =
  now); play/pause at 0.5×/1×/2×/4×.
- **Category jumps** — ◀/▶ to the previous/next event of a category with
  coloured ticks: `utterance`, `correction` (`update_node`/`set_epistemic_state`),
  `decision` (`add_node`/`set_node_kind` → `decision`), `clarification`,
  `promotion` (`link_promoted_object`). `categorizeEvent` considers top-level ops
  only (a `propose_restructure`'s nested staged ops do not mark the timeline).
- **History mode** — scrubbing fetches `replay(id, seq)` (debounced ~250 ms,
  cached per sequence) and renders the map read-only with a "Viewing history
  @seq N · rev R" banner; composer, clarification, proposal, promote, Listen and
  attach are hidden/disabled; the canvas is `readOnly` (selection/pan/zoom still
  work).
- **Compare with now** — `+N added · −M removed · K changed` via
  `diffMaps(then, now)` (tombstoned = absent; layout-only changes excluded).
- **Causal source** — a selected node in history shows its `source_refs`.
- **Restore as branch** — confirm-gated `restore(id, {at_sequence, …})`, then
  navigates to the fork.

Tests: `lib/thinkingMaps/timeLoom.test.ts`.

## Entry points

- **Command palette** — "Thinking Maps" under *Jump to* → `/thinking-maps`.
- **Chat `@brainstorm`** — a `feature:brainstorm` mention (serialized via the
  `@<slug>` fallback in `chipMarkup.ts`). ChatPanel's `isBrainstormInvokeText`
  (mirroring `@tutor`) routes the text into the map flow instead of a chat turn:
  `createMap` (title = truncated seed) → best-effort `/interpret` with the seed →
  `goto(/thinking-maps/{id})`. On failure the text returns to the composer.
