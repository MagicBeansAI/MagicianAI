# Magios Component Documentation

**Path:** `/magios`
**Version:** 0.3.0 (build 202; see [`magios/CHANGELOG.md`](../../../magios/CHANGELOG.md))

Magios is the native iOS companion application for Magician. It is a full Today/chat/attention
client (a native SwiftUI port of the web unified-ui experience) and an ambient OS-integrated
surface (Siri, Action Button, Live Activities). Crate setup and device-run notes live in
[`magios/README.md`](../../../magios/README.md).

Sibling contracts: [ambient-mode.md](ambient-mode.md), [ambient-call-sink.md](ambient-call-sink.md),
[thinking-map-canonical.md](thinking-map-canonical.md), [magican-keyboard.md](magican-keyboard.md),
[monitors.md](monitors.md), [task-verdict.md](task-verdict.md), [muij-graph.md](muij-graph.md),
[today.md](today.md), [voice-call-boundary.md](voice-call-boundary.md),
[app-native-widgets.md](app-native-widgets.md), [execution-panel-delta.md](execution-panel-delta.md).

## Identity and build

- The app, keyboard, share sheet, widgets and Talk control present as **Magican** (glance wordmark
  MAGICAN; Control Center Talk glyph is Outfit `m` plus mic). Bundle IDs use
  `com.magicbeans100x.magican` plus extension suffixes. Deep links accept only `magican://`.
- Enrollment uses the operator's `connect.<zone>` host; that value and its Cloudflare credential are
  not compiled in. Self-hosted connection copy names the host computer and remote backend, never the
  service name.
- The constrained wake spotter arms the in-lexicon `magical` and `magician` spellings of the primary
  definition while display and Siri keep Magican; out-of-lexicon phrases are refused whole.
- Device debug builds: `make ios-debug-build` (two Xcode jobs by default, `MAGIOS_BUILD_JOBS=1..4`).
  Caches live under the Make-exported `CARGO_TARGET_DIR`; use matching `MAGIOS_DERIVED_DATA` /
  `MAGIOS_SPM_DIR` for an isolated build and deploy. `MAGIOS_DEVICE_ID` pins the physical destination
  so an unavailable phone is not silently replaced by a simulator. Deploy reports success only after
  iOS accepts the launch; invalid signatures or missing developer trust exit with an actionable error.
- Codegraph treats `magios/project.yml` as the Swift project manifest; `make graph-index` indexes every
  Swift target including `MagiosTests`/`MagiosUITests`, classifying their calls as `test_calls`.

## Ambient Mode

Magios can **arm a listening window**. While armed, an on-device wake spotter matches the activation
phrase locally — with no networking collaborator, which makes "no audio leaves the device before the
phrase" structural — and on a hit hands off to the realtime voice stack for a hands-free
conversation. An orb in the Dynamic Island is proof of life **and** the disarm control, and the only
stop control reachable without opening the app. **Full contract: [ambient-mode.md](ambient-mode.md)**
(wake spotter, state machine, orb reduction, cross-process disarm, microphone tap, never-deactivate
rule, audio-session safety rails, entry points).

Settings → Ambient Listening owns a local **Dictation**, **Hands-free/FluidAudio** or **Live/realtime**
choice, seeded once from backend media preferences and then independent of in-app Chat and Talk.
Ambient Dictation is a complete turn loop (bounded recording, Dictation STT, canonical agent turn,
Dictation TTS, bounded follow-up capture).

**Ambient Aurora brand kit** (`magios/Shared/`): `AuroraPalette` single-sources phase colours (plus the
task activity's done teal and `controlAccent`), and one `AuroraOrbView` renders identically on the lock
screen, both Dynamic Island densities, the task Live Activity (`MagiosLiveActivity`) and the in-app
voice beacon (`VoiceCallPanel`).

- The live lock screen and expanded island frame the orb in `AmbientLeashRing` — the hard cap as a
  time-driven depleting ring (zero ActivityKit updates), always beside the digit timer, never instead.
- Transcript lines are attributed ("You" or the agent's name; system lines unattributed). Captions are
  **finals only** — every emission is a real ActivityKit publish, so this is a rate-budget decision.
- Stop is paired with **Open** (`magican://ambient`), which reveals the window without starting a turn.
- The ended card shows a receipt ("Listened 42m · 6 exchanges", `AmbientReceipt.line`) and renders
  nothing rather than an invented duration when no honest end was recorded.
- `ContentState` fields (`captionRole`, `exchangeCount`, `endedAt`) are `decodeIfPresent`, and an
  unknown caption role degrades to an unattributed line, so older payloads render.
- System-rendered decorative motion (the halo's `.pulse` while live) is allowed because it degrades to a
  static glow — weaker, not false. Invented durations are forbidden.

> **Cross-cutting invariant:** `VoiceAudioEngine.start`/`stop` take a **required, un-defaulted**
> `VoiceSessionDisposition` (`.release` / `.keepActive`). A session deactivated from an armed ambient
> window cannot be reactivated from the background, so the window dies silently at the *next* wake
> word. In-app calls pass `.release` (unless a window is live); ambient calls pass `.keepActive`. Read
> **[ambient-call-sink.md](ambient-call-sink.md)** before changing any `VoiceAudioEngine` call site;
> a single-line grep for `setActive(false` misses multi-line calls.

## Live Thinking Map

Open from the **Brainstorm** tile in Observe, **Thinking Map** in the drawer, or a leading `@brainstorm`
in Chat (text after it becomes the root thought; a bare command opens capture; quoted or incidental
`@brainstorm` stays ordinary chat). Mention-catalog responses are bound to the session/request epoch so
a late response cannot repopulate the current composer.

Maps are server-authoritative through the canonical Thinking Map API. `LTM.SyncStore` keeps a
last-authoritative cache and a persisted FIFO queue of owner operations, so an open map stays readable
and edits reconcile after reconnect; AI interpretation, promotion, consolidation and Listen attachment
are online-only. Local **Ideas** preferences keep each map's active branch and Canvas/Focus/Outline mode;
a one-time importer migrates the earlier `UserDefaults` maps without deleting backup data. Demo and
UI-test launches use an in-process implementation of the same API. Dictation uses the Magios speech
rail (on-device Apple recognition when allowed); every voice interaction has a typed fallback.

Intelligence is map-scoped: Magios posts the active thought, intent and canonical active-node id to
`POST /thinking-maps/{id}/interpret`; the backend builds bounded map context, routes the dedicated
`thinking_map_interpret` operation (independently routed and metered), translates only the model-safe
operation subset, and applies it through the authority-validating reducer. The active-node id is a
validated request-scoped focus, so select→Continue cannot race the shared-view update. The client never
supplies an agent id and the model never replaces the whole board.

Phone UX:

- **Canvas** (primary): two-axis pan/zoom, recentering, linked cards, provisional dashed AI cards. A
  freshly tapped thought stays active across an older racing projection until its selection is
  acknowledged or the node disappears.
- **Focus**: current thought, lineage and nearby branches within one-handed reach. **Outline**: compact
  hierarchy. Selecting a branch makes it the context for the next thought.
- Node details: edit, connect/disconnect (without changing parent/child structure), subtree removal
  with Undo. **Harvest** summarizes with Markdown copy/share.
- Library: voice/typed capture, search, pin, rename, duplicate, archive, restore, share, lifecycle
  deletion — never mutating unrelated maps.

Suggested nodes stay provisional until the owner acts; owner-only operations (confirmation,
promotion, position locking, restructure decisions) are not expressible by the model. **Break open**
uses its own interpreter intent. When interpretation is unavailable the app shows **Fallback
starters**, a specific **AI unavailable** reason and Retry; the UI-test path is labelled **Demo
starters**, never Offline.

Launch args: `--thinking-map-demo` (populated local map), `--thinking-map-empty`, `--thinking-map`
(library). Ambient Listen attaches the live media session to the open map; Magican Assist can start or
append to a map. Design: Live Thinking Map design.
Implementation: [canonical iOS client](thinking-map-canonical.md) and
[Magician backend](../magician/live-thinking-map.md).

## Core Capabilities

### Today

The Morning Edition ([today.md](today.md)): masthead, realtime wire, operations carousel, Morning
Brief swipe deck and paged Broadsheet over a native port of unified-ui's operating surface (summary
counts, local-date behaviour, analytics pulse, paginated lanes, What Changed, Worth a look, channel
follow-ups, published MUIJ briefings incl. the `Graph` family ([muij-graph.md](muij-graph.md)) with a
JSON fallback, and durable Activity). Polling, realtime refresh, last-good partial data, section
errors, deeplinks and route restoration follow the web lifecycle. Concurrent reads are joined: a
failed primary `/today` stays visible without cancelling valid Worth, Follow-up, Activity, briefing or
Pulse results. Reader-local `today=YYYY-MM-DD` rides every `/today` read.

- **Swipe rails:** card-level (not `List` `.swipeActions`); left = Dismiss, right = Open. Horizontal drag
  suppresses the nested tap. Snooze and backend-advertised source actions (e.g. **Create task**) stay
  compact inline controls. Worth cards use feedback rails (left **Dismiss**, right **Mark useful**) and
  an **Actions** sheet sharing capability resolution with full detail.
- **Attribution:** Useful/Dismiss/Acknowledge carry a unique event id and the exact frozen
  decision/candidate/revision binding from the server; stale, cross-lane or unselected bindings omit
  attribution rather than synthesizing identity. Delivery pages are reconciled against native rows
  before affecting order or telemetry. A continuously half-visible card records an impression only
  after the server-owned dwell interval, with frozen delivery id, page, position, exposure token, scope
  and a stable retry event id.
- **Create reminder** (distinct from Create task): requests Reminders access on first use, writes an
  EventKit reminder, records its identifier through the idempotent resurfacing action, then opens
  Reminders. A failed receipt leaves a bounded device-local pending handoff that survives dismissal or
  relaunch without duplicating; a receipt followed by an open failure becomes **Open Reminders** retry.
- Meeting action items expose the adapter-driven **Create task**; success removes the card and opens
  the task, with deterministic linkage preventing duplicates or resurrection. A meeting action always
  opens native detail. Core Follow-ups have timed snooze presets; channel Follow-ups use annotation
  snooze. Attention does not list follow-ups.

### Observe

Capture meetings from the phone into a per-meeting thread (diarized transcript, rolling + final
summaries, memory) with no Mac. The tab is the web **Command Deck**: header (live status + Refresh) over
a 2×2 KPI grid — **Now & Live**, **Sources on**, **Audio Profiles**, **Notes & Recents** — which are the
only view switchers (remembered per device; `magican://observe?pane=now|sources|audio|notes` opens a
view without starting capture). A **Live** block (this phone's session plus deduped server
bot/host/broadcast sessions and the meeting-ended card) sits above every view.

- **Now:** **Listen** (in-app mic → backend `capture:"client"`), **Join as agent** (Join (Me) / Send
  bot), **Share screen** (ReplayKit extension `MagiosBroadcast`: app audio → diarized `primary`
  channel, mic → hard-"You" `mic` channel, throttled keyframes → client screen observation in the same
  thread; a broadcast arm never starts the in-app mic and clears when the session ends),
  **Brainstorm**; then Upcoming, the `/observe` widget slots, and Recent (newest 30).
- **Sources:** this phone's microphone / notifications / Live Activities with request or Settings
  actions, plus view-only web blocks.
- **Audio:** meeting and listening profiles via `/api/magician/v2/media/preferences`.
- **Notes:** Published Notes over the scoped server pagination contract (search and 5/10/20/50 page
  sizes reset to page one; Previous/Next keep the last good page on failure; review-gated memory
  promotion; rows open in the Notes library) and Audio notes.
- **Ambient UX:** an observation Live Activity + Dynamic Island with a **Stop** `AppIntent` that runs
  while suspended (stops the server session, cascading via `410`); a "now observing" mini-bar on every
  tab; **Start Listening** from Siri, Action Button, Shortcuts and Spotlight (`StartListeningIntent`).

Design: observation design ·
live-test runbook.

### Notes and Audio Notes

- Menu → Notes opens the in-app Markdown library; published task notes open there at their path.
  Terminal task cards expose **Publish to Notes** (card and Actions sheet; the task view keeps a top-bar
  action) using scoped server-owned defaults, serialized taps, and in-place success/failure.
- Chat dictation is ephemeral by default. **Keep dictation recordings** puts the `.m4a` in a protected,
  backup-excluded, bounded outbox; background uploads keep capture-time scope, fail per record, and
  delete only after a matching durable receipt. An in-flight item cannot be discarded into a
  server-save race. Menu → Audio Notes offers search, playback, Retry and confirmed Delete. Ambient
  Dictation and Thinking Map speech are never Audio Notes.

### Apps

- **Launcher:** a server-paginated Apps directory with metadata-only search; non-launchable lifecycle
  states stay visible without exposing records. An enabled view opens in an ephemeral exact-origin
  WebView whose custom scheme reaches only that installation's route, static assets and surface APIs;
  it rejects redirects and cookies and streams under fixed byte ceilings (4 MiB directory envelope).
  Today shows at most eight pinned enabled views and clears synchronously on enrollment/scope change.
  The embedded `magapp:` page syncs through the bounded entity-change REST contract, not a WebSocket.
- **Native widgets:** Today hosts `/` + `primary`, Observe `/observe` + `reviews`; see
  [app-native-widgets.md](app-native-widgets.md). The root tab shell mounts no indicator strip.
- **Custom interactive surfaces:** `AppSurfaceScriptedHostView.swift` hosts a WKWebView at a
  per-installation `magapp-surface://` origin with exact lowercase `blake3:<64 hex>` asset identities,
  deny-all-but-initial navigation, and a closed 8-operation bridge with web-host failure parity.
  - `AppRouteBrowserView` probes `custom-surface-v1/host` at an installation root (bounded timeout);
    declared MUIJ view routes hydrate in the page and are never probed. A minted plan mounts
    `AppSurfaceScriptedWebView` at its digest-keyed entry; `AppSurfaceFailedNotice` replaces a
    torn-down session and `AppSurfaceUnsupportedNotice` closes a plan the client cannot instantiate.
    Every refusal falls back to the launcher WebView. The process-wide operator switch, when off, makes
    the probe refuse.
  - A viewless enabled installation shows **Open app** gated on `custom_surface_entry_count` (0/absent
    hides it).
  - The scheme URL carries the session reference as its first path segment
    (`magapp-surface://<installation>/<session>/<digest>/<member>`) — frames present no headers, and
    relative subresources keep the containing path. The scheme proxy forwards exactly that canonical
    path and refuses a missing, rewritten, encoded or query-augmented address.
  - Renderer crashes get exactly one consume-once recovery reload and POST
    `.../custom-surface-v1/sessions/{session_ref}/reload-note`; a 409 (budget quarantined) runs
    `closeSession`/`onSessionClosed`, so a surface cannot crash-loop past the 3-strike budget.
  - Exhausting the 32-message bridge budget closes the session with the failed notice and an error
    reply. Replies via `evaluateJavaScript` escape U+2028/U+2029.
  - The bridge relays one authenticated `URLSession` request per frame at a time (FIFO): completion of
    sequence `N` (even an operation error) advances `N+1`; a session-level refusal clears pending
    submissions. This respects the server's strict sequence fence.
  - Tests: `magios/MagiosTests/AppSurfaceScriptedHostTests.swift`.

### Magican Keyboard

A system-wide agentic keyboard with a DIY typing core (layers, shift/caps, key balloons, diacritic
callouts, haptics, space-drag trackpad, local suggestions and conservative autocorrect with revert)
and on-demand agentic lanes (✦ key or stationary spacebar hold): **Rewrite**, **Ask**, and skill chips
(**Act** = verification-card-gated `POST /executions`; default Act chip **Add task**; Write chips
Rewrite/Reply/Continue). `KeyboardEditPlanner` never blind-deletes (pre-flight → insert-only fallback →
post-verify → staleness-guarded Undo). Typing works with Full Access off; secure/OTP/number fields
capture nothing. See [magican-keyboard.md](magican-keyboard.md).

### Voice

- **Voice-first chat:** one dominant mic supports tap-start/tap-finish and hold-to-talk with a live
  on-device partial transcript (file→cloud STT fallback). Finishing shows the final transcript, then a
  3 s cancelable countdown auto-sends (`viaVoice`); replies speak back (`auto_speak`) and holding the
  mic barges in. Tap-stop and hold-release share one transcript/countdown/send boundary. Design:
  voice-first chat design.
- **Live voice call:** full-duplex via the **BackendProxied** transport — 24 kHz int16-LE mono PCM as
  binary frames over `/media/voice/{id}/control`; control envelopes (`session.start`, PTT, end) are
  **text** frames. Gated on `GET /media/providers`.
  - Engines: GPT Realtime 2.1 Mini is the native default; others (GPT Realtime 2.1, Gemini Live
    variants) appear in the backend's `display_order` when available. Browser-only direct-WebRTC
    profiles and Gemini Live Translate are not selectable; a stale persisted selection is normalized
    before the call button enables.
  - Captions apply `transcript.assistant.delta` as they stream. `interaction.status: in_progress` drives
    `RealtimeVoiceClient.assistantWorking` ("Working…"); it clears on `idle`, interrupt, or a fresh
    `session.ready`, and only `in_progress` counts.
  - `AVAudioEngine` full-duplex with `.voiceChat` hardware AEC. On each new audio graph, mic forwarding
    is guarded during the first assistant playback and its tail while AEC converges; later replies keep
    barge-in. 45 s readiness watchdog.
  - Magios sends `voice_mode=realtime|hands_free`, the independent turn boundary
    (`server_vad`|`push_to_talk`), and AEC state. Switching boundary or model replaces the provider
    session, rejects its delayed terminal event, and re-arms audio; a PTT hold during the swap applies
    at `session.ready`. Mic-mode and engine switches preserve the audio graph, media session, captions,
    timer and socket. Each media registration is disconnected on teardown.
  - Each start sends a call-scoped override disabling address-prefix gating (no wake-phrase mode on
    iOS calls), without changing Web's scoped preference. Translation mode uses server VAD and disables
    Hold to talk. User captions come from the backend's FluidAudio-first STT chain (one dimmed partial
    row; finals enter chat). Control-socket interruptions reconnect with bounded backoff without
    stopping capture. Finalized transcripts persist in Chat with call-scoped message identity.
  - The floating call panel mirrors web `VoiceCallOverlay` (~four-line caption viewport). Display-only
    call boundary: [voice-call-boundary.md](voice-call-boundary.md).
- **Voice settings:** Replies, Dictation and Live Call sections. First install seeds stable profile IDs
  from scoped backend preferences only after both bootstrap endpoints return schema-valid JSON; later
  choices are device-local. Apple Speech/Voice are labelled **This iPhone**; macOS Speech and FluidAudio
  **Magician Mac**. Starting Live or Hands-free stops chat narration without changing the saved choice,
  and every terminal call path releases that focus.

### Chat & Attention

Native port of web chat/attention: SSE streaming, GFM markdown, tool-result cards, authed images,
artifact actions, session lifecycle, staged attachments, a queue inspector, Stop turn, and
failure-safe mutations. Do is a split Ask/Accept control remembering its permission across Plan; Plan
is the only peer.

- **Engine chip:** installed engines from `GET /plane/engines`; Magician API profiles for Magician/Pi,
  model choices for other harnesses. Each send carries the `harness_engine`, `harness_model` and
  `profile` selected when Send was tapped (device-local in UserDefaults).
- **Mentions** (`magios/Shared/MentionCatalog.swift`): `@tutor`, `@tutor #quick`, `@brainstorm`,
  `@vibedev`, `@vibedev #discuss`. `insertText` stays `feature:<slug>`; `serialized` is the literal
  command (the ASCII space in `@vibedev #discuss` is load-bearing — `#` is a token character).
  Detection and dispatch are server-side.
- **Structured sidecars:** `ChatMessage.presentation` renders only after V1 validation (scalars
  preserved, UTF-8 bounds, HTTP(S) artifact links, ids/MIME ≤ 160 bytes, sizes non-negative
  signed-32-bit), else canonical content. Actions: `copy_text`, `open_url`, `open_task`,
  `open_artifact`, `send_follow_up`, `invoke_server_action`, via the same validated handler as web.
- **Live steps:** iOS sends a stable `chat_turn_id`, mounts the assistant row before the first token,
  and tails the canonical per-turn event projection while streaming and while a correlated delegated
  task is active (lazily hydrated for history). The Steps disclosure shows immediately; **All** opens
  the full log. Reconnects replay with deduplication; terminal task status triggers a REST
  reconciliation. Events are deduplicated and timestamp-ordered. **Open complete result** reconstructs
  versioned complete-value, container and UTF-8 fragment units, never presenting the model's bounded
  projection as complete, and shows only a verified content-hash prefix.
- Persisted and realtime messages share one native projector. Task updates coalesce by task. Escalation
  cards stay unresolved until their HITL response succeeds. Planning clarifications keep question
  correlation id, responder task id and execution id separate; free-text questions collect an answer
  rather than submitting a synthetic label. **Keep Trying** calls the continuation endpoint. Historical
  cards without authoritative option actions open their Attention request.
- **History:** Sessions and Threads tabs with Personal/Automated lanes, 15-item server pages per tab and
  lane; search uses the mixed-history endpoint (120-char cap) with an explicit all-scope. A thread opens
  its newest usable session. `#general` (`is_default_session`) is locked with no archive/delete.
- **Attention** is live-only: Requests, Approvals, Escalations, Failed, All. Counters use backend totals;
  each lane advances its own cursor. Lane chips sit on a UIKit touch layer because SwiftUI button hit
  regions were unreliable on devices. Failed items swipe-to-dismiss. Text and binary WebSocket frames
  are equivalent; reconnect is suppressed after the view disconnects.
- **HITL modals:** a distinct shape per input type (eleven). `tool_authorization` and
  `sandbox_override` name the subject verbatim in mono with the broken policy and covered roots, refusal
  first, and one button per offered grant (`allow_always` writes the session allowlist). Matches web
  [unified-task-panel.md](../unified-ui/unified-task-panel.md), *Answering an ask*.

### Tasks

Native port of web `/tasks` with three lanes (Tasks / Monitors / Internal tasks).

- **Regular lane:** presets (All / Inbox / Today / Overdue / Running / Completed) computed server-side
  over the whole pool and badged from `counts` (no badge when unreported); tag chips, search, status
  ledger. Pages are 50 (`limit`/`offset` on `/v3/tasks` and `/v3/tasks/internal`) with a "Load more · x
  of N" footer; a legacy backend without the envelope degrades to one full response. Requests carry
  `view=<lane>` and `today=YYYY-MM-DD`. `TaskFilter.matches` runs locally only where the server has no
  answer: a selected tag (request drops `view=`), a binary without `counts`, and the lane-change round
  trip. The 5 s completion grace is client-side (`gracedRows`), starting from the tick.
- **Internal lane:** every `/v3/tasks/internal` row (web's Any-status default) with its own status/agent
  filters, search and sort; regular presets and tags hidden.
- **Cards:** the web state-action matrix, **Result** when web `hasFinalResult` holds (primary on
  completed cards → Output → Result), Reset to Ready only for failed/cancelled tasks that have run, a
  **↻ Recurring** chip (`TaskCronDescription`) instead of a raw cron pill, and an Actions sheet.
  Swipe right = Complete / Mark not done; left = confirmed Cancel/Delete; execution controls are never
  inferred from status for swipes.
- **Detail:** Overview, Run, Output, Plan, History tabs, with the nine-state verdict
  ([task-verdict.md](task-verdict.md)). Output: Result, task-scoped **Deliverables**, then a collapsed
  **Intermediate artifacts & evidence** grouped Direct / Delegated / Persisted. Run groups delegated
  children (`run.delegations`) into collapsible envelopes (per-device Grouped | Chronological) and
  streams exact-execution `ExecutionPanelDelta` snapshots, bounded to the newest 200 events. History
  shows `started → ended · duration`. Historical runs are inspect-only; Pause/Resume/Steer/Stop bind to
  the server-authorized active root execution. Reset is a checked server lifecycle transition.
- All task mutations carry the workspace-bound device bearer, including routes with no scope selectors.
- **Monitors:** full web parity; see [monitors.md](monitors.md).

### Tutor overlay

Photos and single-image Share Extension entries feed a full-screen screenshot view; Tutor shapes arrive
over the realtime bus and are projected, animated, captioned and narrated via the selected TTS. Narrated
shapes reveal at playback start, and the timeline waits for speech and drawing before advancing.
Dismiss atomically cancels the chat turn and its Tutor run (keeping the session).

- Shape `type`s are not hardcoded: `TutorPrimitiveRegistry` loads declarative JSON recipes (bundled
  fallback + `GET /api/magician/v2/tutor/primitives`) rendered by a generic interpreter
  ([authoring reference](../magician/tutor-primitive-recipes.md)). The bundle is regenerated by
  `scripts/magios-bundle-tutor-primitives.sh`.
- Text-sized recipes measure via `RecipeInterpreter.measureLabel` and expose `text_w`/`text_h`/
  `text_rise`. **`text_rise` is half the glyph box here, not the font ascent**, because labels draw with
  `.leading` = `UnitPoint(0, 0.5)` (vertically centred) where web anchors on the baseline; a shared
  recipe writes `y-text_rise-pad`. [`callout.json`](../magician/tutor-primitive-fixtures/callout.json)
  pins expression evaluation only. `arc` accepts optional `rx`/`ry` (fallback `r`) for `cone`/`sector`.
- `TutorOverlayView` paints fills, strokes, then text as a **stable** sort; box labels wrap via
  `TutorShapeRenderer.wrapLabel`.
- **Explain deeper** (offered only after the latest step finished drawing and speaking, after
  completion, or during replay) composes a normal `@tutor` turn with a fresh `chat_turn_id`; the sender
  accepts it unchanged. Late events from the prior lesson are ignored.
- Voice `Tutor …` variants open the source-free blackboard after the live audio graph closes; Screen
  Tutor and App Copilot are spoken as unavailable on iPhone. A locked request speaks the unlock
  instruction and is never queued.

### Share, Siri and system surfaces

- **Share sheet:** **Magican** (Share Extension) durably stages text, URLs, images and files in Chat.
  **Magican Assist** (full-screen Action Extension): text → Rewrite/Summarize/Reply/Shorten/Clarify/
  Continue; image → Start Tutor, Add to Chat, or on-device Vision OCR then writing help; webpage →
  Summarize Page, Ask Sam, Add to Chat, as task-backed Internal execution (headless browser first),
  with a **Check Status in Magican** handoff to the exact task. Payloads are written to the App Group
  before any best-effort app-open.
- **Siri:** App Intents publish app-qualified phrases for the scoped Crew definition marked
  `is_primary`. Apple requires the app name in every App Shortcut phrase, so `suggestedEntities()` and
  `SiriPhrasePresentation` filter the redundant *Ask Magican using Magican*, leaving **Ask Magican**;
  `entities(matching:)` stays unfiltered. The canonical name is fetched on every foreground activation,
  stored in the App Group for out-of-process execution, and pushed via `updateAppShortcutParameters()`.
  No assistant name is compiled in.
- **Live Activities:** the task Live Activity filters realtime events to its `task_id`; a watchdog ends
  a stalled activity. A task-scoped ActivityKit push token keeps it updating while the app sleeps
  (ambient and observation activities stay local-only). Teardown drains an in-flight token
  registration before deleting its route; an unbound or failed dispatch cannot adopt another run's
  events.
- **Magican at a glance** (WidgetKit, Home/Lock sizes): Needs You → active work → Talk, from a cached
  last truthful projection so offline refresh never invents an all-clear.
- **Talk to Magican:** widgets, Control Center/Action Button control, Shortcuts and Settings all invoke
  `ArmAmbientIntent`: the tap opens the foreground audio session and starts the first turn, then the
  window returns to wake spotting. The Lock Screen / expanded island **+30 min** action extends every
  displayed deadline together up to the eight-hour ActivityKit ceiling. `StartVoiceIntent`, old pending
  values and bare `magican://voice` migrate to this path; `?mode=dictation|hands_free|live` keep
  one-shot modes.
- **Appearance:** one row per theme family plus a sun/moon control; exact theme IDs stay web-compatible.
  Colours resolve through `ThemeManager` semantic tokens; accent foregrounds follow web
  `--text-on-accent`. All 22 variants mirror unified-ui's brand, display, body and mono roles (bundled
  Outfit, Manrope, Geist Mono, Bricolage Grotesque, Pixelify Sans, Permanent Marker, Lilita One).
- **Storage maintenance:** Settings shows Automatic maintenance (Channel Assist/Feed state, message,
  last completion); the scoped `/storage/maintenance` read refreshes every 10 s only while mounted and
  active, and failures clear stale status.

## Networking

Magios routes all intelligence to the Magician backend over the customer-owned mobile origin enrolled
at runtime (`connect.<zone>`, not compiled in). `scripts/ensure-magician-tunnel.sh` configures
**path-based routing**:

- `/api/*` — REST **and** WebSockets — to the selected backend (`localhost:3002` native, or the container
  host port, normally `localhost:13002`). Cloudflare proxies WebSocket upgrades natively.
- `/host/*` — the Tauri Desktop Host Gateway (`localhost:3017`).
- `/health` — the backend's aggregated health (Magican, Magicutor, Tauri reachability) for the Settings
  service ledger.
- Everything else — `404`.

`scripts/ensure-connect-access.sh` gates the origin behind Cloudflare Zero Trust Access and writes the
origin, Access audience and issuer to runtime env. Magician Settings issues separate iPhone and Android
five-minute QRs; the exchange is the only path-specific bypass and returns the outer credential plus a
revocable per-device token. Magios probes an authenticated route before committing the atomic Keychain
profile, then attaches both layers via `MagicianAccess`, which derives the device bearer and outer
credentials from one profile snapshot per request (no mixed-authority requests during rotation). Legacy
Cloudflare fields migrate lazily into Keychain; the plaintext is deleted only after exact read-back.

### Scope (`principal`/`workspace`) is mandatory on every call

The backend is **fail-closed** on scope: REST reads return empty for the wrong scope, the realtime
WebSocket (`/api/magician/v2/realtime/ws`) is rejected outright (`resolve_required_scope`) and every
event is filtered by principal/workspace (`event_visible_to_scope`). So every REST, SSE, WebSocket and
media call carries authorization from `MagicianAccess`, whose opaque device bearer is bound to
principal/workspace by the server-side pairing record. Scope is never sent in headers, queries or TTS
bodies. Credentials attach only to the exact enrolled Magician origin.

> The desktop Host Gateway (`/host/*`) stays desktop-only. iOS Tutor turns use
> `source_surface=ios_tutor_overlay`, so `screen-draw` shapes arrive as `tutor.draw.shape` realtime
> events and never call the gateway.

## Signing

The Apple Team ID is not in tracked files. `magios/project.yml` wires `configFiles` → tracked
`Signing.xcconfig`, which optionally includes gitignored `Signing.local.xcconfig` (copy
`Signing.local.xcconfig.example`, set `DEVELOPMENT_TEAM`); the overlay survives `xcodegen generate`.
Without it, simulator builds work and device builds prompt for a team. The app and extensions must
resolve to the team owning their App Group; remove conflicting target-level Team overrides.

Debug device builds use push-free `Magios/Magican.entitlements` (Personal Team signing works). Release
and `make ios-debug-build-push` / `make ios-debug-run-push` use `Magios/MagicanPush.entitlements`, which
needs a paid team with Push enabled. `MAGIOS_DEVELOPMENT_TEAM=<team-id>` overrides the team for one
`scripts/magios-run.sh` run. A push-free build keeps widgets and local Live Activities but labels remote
alerts unavailable.

## Setup

See [magios/README.md](../../../magios/README.md) for checked-in Xcode project build instructions.

## Tests

- **`make test-ios`** — the **blocking gate** invoked by root `make test` on macOS (skipped without
  Xcode). Runs `MagiosTests` with coverage and skips `MagiosUITests`. Provisions and checksum-verifies
  the gitignored Vosk framework/model; unit tests isolate HTTP via a mock URL protocol.
- **`make test-ios-ui`** — on-demand, non-blocking XCUITest smoke suite
  (`-retry-tests-on-failure -test-iterations 3`, no coverage). Split out because under full-suite load
  XCUITest's late SIGTERM of a slow-to-terminate app is misreported as a crash — framework timing, not
  an app defect. Each UI test launches offline with `--ui-test` (`isUITestLaunch` skips backend fetches,
  sockets, health polling, splash and animations) and terminates the app in `tearDown`.
- **`make test-ios-live-*`** — opt-in physical-device lanes (`MAGIOS_DEVICE_ID`,
  `MAGIOS_LIVE_TEST_HOST`, host verified in Settings first); `test-ios-live-chat` performs real
  inference and checks reply persistence after restart. See
  [container-integration-harness.md](../scripts/container-integration-harness.md).
- **`make verify-magios-vosk`** — read-only check at the start of `make check-all` (archive checksums,
  framework manifest, model sentinel); remediation is `make setup-magios-vosk`.

**Report runner.** Each `make test-ios` run writes `coverage/ios/latest.html` (raw `.xcresult` and
stderr logs under `coverage/ios/results/`; `IOS_TEST_REPORT_DIR` relocates) and a JSON sidecar.

- Every Xcode attempt gets isolated temp/cache/DerivedData under `IOS_TEST_WORK_ROOT` (default
  `/private/tmp/magician-magios-tests`), including an attempt-local `HOME`, `CFFIXED_USER_HOME` and
  compiler caches; inherited XPC, sandbox and `DYLD_*` metadata is removed. Only the durable
  CoreSimulator device registry/runtime map, Xcode UserData and MobileDevice dirs are bridged from the
  real home (never CoreSimulator `Caches`/`Temp` or DerivedData).
- The runner resolves the real Xcode binary and enters the user's bootstrap via `launchctl asuser`. A
  missing Aqua/user bootstrap is reported explicitly and not retried. Otherwise it retries at most once,
  only for the SIGABRT + FSEvents + `DARWIN_USER_CACHE_DIR` I/O failure with no result bundle; test
  failures and ordinary Xcode failures are never retried.
- Post-XCTest `simctl diagnose` descendants are capped at `IOS_TEST_DIAGNOSTIC_TIMEOUT` (30 s default)
  while `xcodebuild` finalizes the bundle.
- One runner owns the report directory (`IOS_TEST_LOCK_TIMEOUT`). The prior report moves to
  `previous.html`; a new `latest.html` is published atomically only from this run's bundle, so a missing
  or stale bundle cannot masquerade as fresh coverage. Aggregate `make test` links the immutable
  `reports/Magios-<run>.html` via a correlation ID and manifest.
