# HITL + Realtime Events UI Pipeline

Reference for the human-in-the-loop modal flow and the realtime event surfaces (`/events`, `EventStreamCard`, `event-taxonomy.ts`).

## Layout

```
src/lib/
├── hitl/
│   ├── adapters.ts            FeedItem → HitlRequest projection
│   ├── respondToHitl.ts       Modal lifecycle + response submission
│   └── types.ts               HitlRequest / HitlResponse contracts
├── realtime/
│   ├── EventStreamCard.svelte Live-tail card (used by /events + ExecutionPanel)
│   └── event-taxonomy.ts      GENERATED — TS mirror of Rust taxonomy
├── stores/
│   ├── attentionPromptStore.ts Modal-host store for AttentionPromptModal
│   ├── confirmationStore.ts    Generic confirm()-style modal store
│   └── pendingHitlStore.ts     Live count of pending HITL items per scope
├── magician/components/
│   ├── AttentionPromptModal.svelte
│   ├── ConfirmationModalHost.svelte
│   └── OpenInStreamLink.svelte
└── routes/(app)/events/+page.svelte
```

## HITL Flow

The "user is asking the bot to ask the user something" path:

1. Backend emits a `UserRequestPending` (legacy) AND a canonical `HitlRequested` event onto `RuntimeTransportBroadcaster`.
2. The AttentionBar (or any other surface watching the feed) renders the request via `hitlRequestFromFeedItem` from `adapters.ts`.
3. Operator clicks Respond → calls `respondToHitl(request, headers?)`.
4. `respondToHitl` projects the request shape to a prompt kind (`text` / `multiline` / `password` / `choice` / `guidance` / `form` / …), opens `AttentionPromptModal` via `attentionPromptStore.requestAttentionInput(...)`. `form` renders stacked fields in `HitlPromptFields` with per-field Skip and Skip all.
5. For `confirmation` requests with no explicit `options`, two synthetic choices are injected (`{id: 'confirm'}` and `{id: 'deny'}`) using the schema's `confirm_label` / `deny_label` so the choice modal has a submit path. Without this synthesis the modal would render an empty radio group and `canSubmit` would stay false forever.
6. On user submit, `mapPromptResultToHitlValue` converts the modal result back into the typed `HitlResponseValue` shape and POSTs it via `postHitlResponse`.
7. Backend emits `UserRequestResolved` + canonical `HitlResolved`. Both are caught by `pendingHitlStore::isResolutionEvent` so the badge count decrements.

## `pendingHitlStore`

Single source of truth for "how many HITL items is the system waiting on right now". Subscribes to `/api/magician/v3/events?category=hitl&user_relevant=true` (note: **no** severity filter — pending requests ride at `attention`, resolutions at `info`; pinning the URL would silently drop every resolve and the count would only ever grow).

- `pendingHitlCount`: derived `Readable<number>` — drop into any badge surface (TopBar Ops, Internals drawer, AttentionBar summary).
- `pendingHitlEntries`: derived snapshot for debug.
- `ensurePendingHitlBridge()`: idempotent. Stashes the scope-store unsubscribe in a module-level handle, so multiple TopBar mounts (layout swap, hot-reload, theme cycle) don't stack duplicate scope listeners.
- `teardownPendingHitlBridge()`: aborts the live-tail fetch, clears the pending map, releases the scope subscription.

Resolution detection is by `event_type`: canonical `HitlResolved` / `UserRequestResolved` plus any `*.resolved` / `*.responded` / `*.expired` / `*.cancelled` / `*.dismissed` suffix. Correlation key tries `correlation_id` first (canonical), falls back to `pause_state_id` / `approval_id` / `clarification_id` / `event_id` / `id` so the legacy + canonical dual-emit deduplicates into a single Map entry.

## Bot-auth ping cadence (`attentionStore`)

`attentionStore` fires a **fire-and-forget** `GET /api/magician/v2/bots/auth` ping whose response is discarded — its only job is to drive the backend `AuthHitlBroker` to compare each bot's auth snapshot and emit `HitlRequested` / `HitlResolved` on a transition (those land in `pendingHitlStore` like any other HITL source). Because the backend already caches the live `gws auth status` probe (~10 min), pinging on the full 15s attention cadence buys no extra freshness, so the ping is **throttled to `AUTH_PING_INTERVAL_MS` (60s) and skipped while `document.hidden`**, independently of the `/feed/attention` data fetch (which keeps its 15s + websocket cadence). A genuinely-broken bot still surfaces via its needs-auth sidecar on the next ping once the tab is visible.

## Spine lifecycle affordances (`ConversationSpine`)

The conversation spine fills the two silent gaps in a run's lifecycle so neither reads as "nothing is happening":

- **Start gap** — dispatched but no coding event yet (`!historical && runKey && streamState !== 'error' && !synthesizing`, zero cards): a spinner + **"Starting shortly…"** replaces the misleading "send a request below" empty copy (the request was already sent).
- **End gap** — the build/plan stream has closed but the agent is still synthesizing the final result (the `synthesizing` prop, fed from the run's `synthesisPending`): a **"Synthesizing the result…"** footer renders below the finished cards, mirroring the rail's "Synthesizing" status so a still-working run doesn't look done.

Both spinners are disabled under `prefers-reduced-motion`.

## Stopping a run (`controlRun` → `/vibedev/runs/{run_id}/control`)

The cockpit's "■ Stop" control (`VibeStudio.svelte`) POSTs `{action:'stop'}` to the run-control endpoint. The backend stop is terminal in every state — it aborts a live Pi turn if one is registered, else falls back to cancelling the resolved execution tree (`cancel_execution_by_id`), persisting a `cancelled` outcome even when the runtime is gone. Because of that fallback, Stop is gated on **`canStop`** — a *selected* run that is non-terminal **or still synthesizing** (`canStop = Boolean(stopRunId) && (!taskTerminal || runSynthesizing)`, `stopRunId = liveRunId ?? activeVibeTask?.id ?? null`, keyed on the **present run object** — not the route-derived `activeTaskId`, which lingers in the URL after a run is deleted and would otherwise show Stop + the spine's "Starting…" for a run that no longer exists) — rather than the narrower `runLive` (which also requires a live stream + an event within the last 10 min). The `runSynthesizing` arm matters because a run can report a **terminal** task status (e.g. `completed`/`failed`) while a synthesis-pending execution is still outstanding — the rail shows "Synthesizing" for it, so Stop must stay available. This keeps Stop available for stuck/looping, synthesizing, or stream-dropped runs, which is exactly when it's needed.

The same present-run gating clears the **right-panel run telemetry**: `runMeta` (cost / context% / tokens / model, fed from the persistent coding-spine store) is null unless there's a present run (`activeVibeTask ? pickMeta(…) : null`), and the status-bar cost/budget chips are gated on `meta` — so a deleted or previous run's numbers don't linger once no real run is selected.

## `event-taxonomy.ts`

**Generated file — do not edit by hand.** Mirror of `magician/src/magician_v2/realtime_events.rs::taxonomies!` macro + `AGUI_EVENT_TAXONOMY` const, produced by `cargo run -p magician-event-taxonomy --bin event_taxonomy_dump`. Run `make event-taxonomy-codegen` after editing the Rust taxonomy. The fast `make event-taxonomy-check` gate runs on every test/build and fails loudly when the embedded `// SOURCE_HASH:` marker no longer matches a freshly-computed hash.

When the V3 HITL lifecycle authority is ready, app-owner notification request and resolution events cannot enter through the generic transport emitter. They must use the dedicated generation-bound APIs that persist and authorize the exact lifecycle transition before publication. A change to this source-level transport invariant advances the generated file's `SOURCE_HASH` even when the taxonomy rows themselves do not change.

See `docs/process/ai-doc-hooks.md` § Event Taxonomy Codegen + Drift Gate for the operator workflow.

The TS file exports:

- `EVENT_CATEGORIES` / `EVENT_SEVERITIES` — frozen arrays of valid string literals.
- `EventCategory` / `EventSeverity` — derived TS types.
- `EventTaxonomy` — `{ category, severity, user_relevant }` interface.
- `EVENT_TAXONOMY` — `Record<eventType, EventTaxonomy>`. Includes both typed `RuntimeTransportEvent` variant names (`MessageProcessingStarted`) and AGUI envelope event types (`'tool.call.started'`, `'reasoning.content'`, `'plan.snapshot'`, etc.).
- `taxonomyFor(eventType: string)` — safe-fallback lookup; unrecognized event types return `{category: 'observability', severity: 'info', user_relevant: false}`.
- `KNOWN_EVENT_TYPES` — flat array of every entry in `EVENT_TAXONOMY` (filter chip options).

## `EventStreamCard.svelte`

Reusable live-tail of `GET /api/magician/v3/events` (NDJSON over chunked HTTP). Used by `/events` (full density, no scope filter) and ExecutionPanel Activity tab + Internals drawer (compact density, scoped to a single execution).

Key behaviors:

- **Local-time everywhere**. Row time cell shows `MMM DD HH:MM:SS.mmm` in the user's locale. Tooltip on hover gives the full local datetime + tz abbreviation. Detail/peek pane has a "time (local)" row above the raw `timestamp_ms`. The raw JSON dump runs through `localizeJsonTimestamps` so embedded ISO-`Z` strings inside payload fields are rewritten to local-zone ISO with offset.
- **Horizontal scroll for full event line**. Cells use `white-space: nowrap` (no ellipsis); row uses `width: max-content; min-width: 100%`; container has `overflow-x: auto`. Long previews push the row past viewport and surface a horizontal scrollbar.
- **Sticky column headers** with `position: sticky; top: 0`. The list container drops its `padding-top` so the header sits flush at scroll origin.
- **`__events_partial__` and `__events_lagged__` sentinels**. The cross-scope backfill emits `__events_partial__` when it hits the 50k scan cap; the live-tail emits `__events_lagged__` when the server-side broadcast channel drops events because the consumer can't keep up. Both surface as banners (not rows) so operators see the gap.
- **Debounced text-input filters** (`event_type`, `agent_id`, `search`) — 300ms debounce on `on:input`. Chip toggles (category/severity) reconnect immediately because their server-side narrowing materially reduces wire traffic.
- **Anchor row highlight**. Optional `anchorTimestampMs` prop — after the first batch lands, the row closest to that ms is scrolled into view and briefly highlighted. Used by deep-links from feed/step/attention surfaces.

## Task status card — "Preparing final result…"

The `task_status_update` card (`ChatPanel.svelte`, via `taskStatusVisual`) renders
a distinct spinner + "Preparing final result" state when the card content carries
`synthesis_pending: true`. This covers the window where a task's execution has
finished but its user-facing result is still being synthesized async on the
backend (the backend holds the visible status at "running" meanwhile, so without
this the card would read as a bare "Running"/"Live"). `ChatMessageContent.synthesis_pending`
is optional (`boolean`); absent/false renders the normal lifecycle visual. The
flag is set by the backend `ChatChannel` from the task's pending-synthesis state —
see `chat-mode.md` ("Preparing final result…").

## Off-call task-completion TTS (`taskCompletionSpeech.ts`)

When a task finishes and no live voice call is running, the terminal
`task_status_update` card carries a `speech_tts` line (synthesizer-authored).
`chatStore.handleChatMessageReceived` (the live WebSocket path — history replay
does not pass through it, so old completions never re-speak) calls
`maybeSpeakTaskCompletion(message)` on each newly-appended message. It reads the
line aloud via the browser TTS engine when: auto-speak is unmuted
(`ttsStore.prefs.autoSpeak` + `userInteracted`) AND no `voiceCallStore` call is
active (a live call speaks the terser `speech_live` over the voice channel, so
this path stays silent to avoid double-speaking). If the tab is hidden it raises
a desktop notification (web Notification API — also native inside the Tauri
webview) and speaks on the next `visibilitychange`.

## Wake-word calls start in push-to-talk + live PTT toggle

`pushToTalkMode` (in `realtimeVoiceClient.ts`) defaults to `true`, and PTT mode
disables the mic track at connect. The store is the **single source of truth**:
the call's connect mode (`connectProvider` reads `get(pushToTalkMode)`), the
on-screen PTT buttons (`$pushToTalkMode`), and session rotations all follow it.
`fireWake` flips it **on** (`setPushToTalkMode(true)`) before `startVoiceCall()`,
so a wake-started call comes up in push-to-talk — connected fast, but mic muted
until you hold the PTT chord (Left ⌃+Left ⌥) or on-screen PTT button. This keeps
wake from opening a hands-free mic that would stream/bill the room; switch the
live call to hands-free any time from the call UI. Because the store drives both
the connect mode and the buttons, they always agree (an earlier per-call
`handsFree` override diverged from the store and was replaced with store-as-truth;
the original default flipped wake to hands-free, since reverted).

`setPushToTalkMode` applies to a **live** call: it calls the provider's optional
`setTurnDetection` (OpenAI/DirectPeerToPeer only) to re-send a `session.update`
rebuilding `turn_detection` (PTT → none, hands-free → profile VAD) and re-arm/mute
the mic — no reconnect. A rotation re-reads the store, preserving the toggled
mode. Every PTT toggle button (`VoiceCallOverlay`, `DesktopVoiceCenterStage`)
calls `setPushToTalkMode`, not `pushToTalkMode.set` directly, so the reconfigure
always runs.

Wake resume after a call has a **cooldown** (`WAKE_RESUME_COOLDOWN_MS`, ~2.5 s):
when a call returns to idle, the wake listener is re-armed via a timer rather than
immediately, so the tail of the just-ended conversation (the user's "bye", the
assistant's trailing audio) doesn't re-trigger wake and spin the call straight
back up. The timer is cancelled if a new call starts first.

## Push-to-talk: one mode-aware Left-Control+Left-Option chord

`wakeWord.ts` also exports the push-to-talk controller. A single chord (**Control
+ Left Option**) is mode-aware against the universal Call/Dictate switch
(`voiceModeStore`): `pushToTalkPress()` starts/engages a **live** call
(`engagePushToTalk`, cold-starting it in PTT mode if none is live) or starts a
**dictation** (`recordingTrigger`); `pushToTalkRelease()` commits the live turn
(`releasePushToTalk`) or stops the take (`recordingStop`, registered by
`VoiceControl` alongside the existing start trigger). The mode is captured at
press so a mid-hold switch releases on the same path.

The in-page listener (`installPushToTalkHotkey`, ref-counted, mounted by
`VoiceControl`) engages when both `ControlLeft` and `AltLeft` are held and
releases when either lifts, **deferring engagement ~120 ms** and cancelling if a
third key follows — so a `Ctrl+Option+<key>` shortcut doesn't trip it. A two-key
chord (not a bare modifier) is used so it never shadows ⌥-key typing. On the
desktop the native CGEventTap (`voice_gesture.rs`) owns the same chord and keeps
the OS event; the web mirrors the active mode via `invoke('set_ptt_mode', { mode
})` and the in-page listener is disabled inside the Tauri webview, so the two
surfaces never double-trigger. The legacy Hold Right ⌥ live gesture is retired.

## The "typing" bubble only shows for a live/awaiting turn

The `chat-turn-typing` bubble (the "assistant is working" indicator, `ChatPanel.svelte`)
renders for a user turn that has no recorded assistant response yet — but it is
now gated on the turn being the **live in-flight turn** (`messageTurnId === liveTurnId`)
**or** having a **pending HITL** request. Without that gate, a turn that ended
without a text reply (e.g. a voice turn where the model replied with only a tool
call, whose activity lives under a separate task turn id, not the user turn id)
showed a phantom empty "working" bubble that never resolved.

## Empty assistant turns are hidden

The chat render (`ChatPanel.svelte`) skips the bubble for an assistant turn whose
rendered text (`getMessageText`) is empty **and** that has no folded in-turn
activity (`attachedActivityMessages` / `taskExecutionGroups` both empty). This
covers (1) streaming placeholders before the first token — `<ChatTurnProgress />`
already shows the working state — and (2) final tool-call-only turns, e.g. a voice
turn where the model "responded" by acting with no spoken text. It is
**activity-aware on purpose**: task cards fold *into* the following assistant
message (`attachInTurnActivityToAssistantMessages`), so a blanket
message-level filter would strip a card's fold target and hide the card; the
render-level skip leaves any turn that folded a card untouched. As a second
guard, the bubble's text + speak-button only render when `getMessageText` is
non-empty — so a turn that DID fold a task card (and thus isn't skipped) shows the
card without an empty text area above it.

## Response source routing (attention cards)

`hitlRequestFromFeedItem` (`lib/hitl/adapters.ts`) decides which dispatcher
arm a card's response POSTs to. It **prefers the backend-stamped explicit
`metadata.source`** over the `resolveSource(attention_kind)` heuristic: the
attention projection now stamps `source` on every `V3AttentionSummary`
(`diff_approval` / `user_request` / `clarification` / `plan_approval`), so a
"code apply" (`diff_approval`) card routes to the apply/reject arm instead of
the `agentic` catch-all (which never applied the proposal, so the card
reappeared). `resolveSource` keeps explicit arms for those sources as a
back-compat fallback for older cards that lack `metadata.source`. See
`docs/components/magician/hitl-attention.md` ("Card resolution / reappearance
contract") for the backend id-pairing + durability invariants.

Agentic/escalation responders treat both the legacy `404` and the current
`410 pause_state_gone` response as idempotent success: the requested pause is
already resolved or expired, so the local pending and Attention copies are
dropped. This soft-success rule is intentionally source-scoped; an unrelated
clarification `410` remains an error.

## Failed-item dismissal (server-side, v0.6.929)

Failed ("terminal report") attention items are non-actionable and the backend
re-projects them every poll, so dismissing one must be remembered durably.
`attentionStore.dismissFailed(id, feedItemId?)` (`lib/stores/attentionStore.ts`)
now POSTs the raw **`FeedItem.id`** to `POST /api/magician/v2/feed/attention/dismiss`
(scope in the query string, same convention as the GET) so the server filters
it out of every lane for all devices — NOT the correlation-based dedupe key
(`row.key`), which the server can't match. The `/attention` page threads
`item.id` onto the row as `feed_item_id` for this. The legacy optimistic
in-memory + `localStorage['attention:dismissed-failed']` removal is kept for
instant UX and as a fallback against an older backend, but the server store
(`AttentionDismissedState`) is the durable source of truth. See
`docs/components/magician/hitl-attention.md` ("Durable dismissal of failed
items").

## Backend pairing

The Rust counterparts are documented in:

- `docs/components/magician/v2-websocket-events.md` — full event reference, payload shapes, AGUI envelope flow.
- `docs/components/magician/chat-mode.md` — chat dispatch + delegation event fan-out (which feeds the events surfaces).
- `docs/components/magician/v3-complete-flow.md` — chat-inline delegate path (live-tail interaction).
