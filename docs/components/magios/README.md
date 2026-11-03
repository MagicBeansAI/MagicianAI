# Magios Component Documentation

Device builds use the configured Xcode team; a matching imported App Store Connect key supplies provisioning authentication when present, with credentials retained outside Git.

Settings search and About version rows use the shared product and backend labels.

Empty chats mirror Web's local greeting, four editable prompt starters and three recent conversations, using themed compact cards, scrolling and a single column at accessibility text sizes; starter taps focus a draft without sending.

History session rows show a compact Working badge for current chat/task activity, refreshed only while the panel is visible in the foreground.
In a task's deep panel, a failed tool row shows why it failed under its title (two lines, Show more when cut; `TaskActivityFailure` in `DeepWorkPanel.swift`).

Live calls occupy the Chat and VibeDev composer slot, retaining queue/ask controls and a Type message option; ending a call restores its previous voice/text mode and draft.

The split voice control persists Dictation, Live or Hands-free on its mode side and opens settings from its chevron; the central button starts the selected flow with microphone, waveform or headphone icons. God mode exposes an auto-approve chip and a themed composer aura; enabling still uses the existing confirmation and server policy.

Chat model/harness choices and unsent drafts persist per server, account, workspace and chat. The chat’s three-dot menu opens Tasks (thread-filtered Work), Thread settings (name and saved memory), Files, and Models (per-model and cumulative tokens, cache and cost); unknown usage stays explicit. Dictation and spoken replies carry the initiating chat for billing. File lists show uploads/task artifacts, sizes and confirmed removal of chat copies, retaining task originals under Work; relative links use the originating chat. Quality budget controls accept zero as Unlimited.

Chat composers expose God mode for the current chat and its Settings default; Chat and VibeDev expose backend-wide Super God mode for one hour or until turned off, with confirmation, expiry status and a turn-off action. Mode changes use owner-authenticated endpoints and stay bound to the connection whose settings were shown.

Spoken results lead with findings and keep a short verification qualification; checker diagnostics stay in the answer's disclosure. Chat detail sheets stay bound to the connection that opened them.

Chat and VibeDev consume the server's [model catalog](../magicllm/model-catalog.md).
Chat reads `/chat/models?engine=magician|pi` and sends `model_selection` with an
explicit engine on streaming, queued and concurrent-voice requests. CLI chat
engines retain their native model field. VibeDev reads `/coding/models`, filters
models by engine and sends `coding_choice` on new/follow-up runs. Readiness is
independent of model availability.

Pickers show product provider names and supported native thinking steps/switches.
Auto uses the model/workload or engine default without escalation; context is
read-only and token allowances stay internal. Preferences persist model/thinking
and coding engine identity. The frozen historical converter imports old selectors
once; typed preferences win. Missing/malformed/unavailable explicit choices remain
saved and block new model-based sends until replaced. There is no profile-list
fallback. Catalog requests are guarded against stale engine/session responses.

Crew editors derive lane and coding choices from routing/model descriptors.
Pins retain canonical handles and can explicitly follow global routing. Slider
drags preview locally and commit the final step on release. Quality uses the same
controls but admits only bounded, priced routes and independent review families;
the existing preview seals exact generated handles.

Selectors use themed bordered boxes and searchable full-height sheets. Choices
are snapshotted with stable IDs while open so refreshes cannot reset scrolling;
gestures prefer list scrolling over detent changes. All slider stops are tappable,
Auto/selected labels use rounded squares, and VoiceOver commits adjustments directly.
VibeDev actions and project/mode/crew choices share 8-point rounded squares and
opaque themed secondary fills. Wide actions are 38 points tall with 44-point tap
targets; segments have a 32-point visible selection with 44-point targets. Custom
iOS 26 toolbars supply their themed background instead of a glass capsule. A VibeDev project whose `health` is not `ok` shows its issues (and a confirmed `git_init` button, `POST /vibedev/projects/{id}/git-init`) on its header and new-run sheet, and a ⚠ glyph in lists and pickers.

**Path:** `/magios`
**Version:** 0.4.4 (build 215; see [`magios/CHANGELOG.md`](../../../magios/CHANGELOG.md))

Magios is the native iOS companion application for Magician. It is a full Today/chat/attention
client (a native SwiftUI port of the web unified-ui experience) and an ambient OS-integrated
surface (Siri, Action Button, Live Activities). Crate setup and device-run notes live in
[`magios/README.md`](../../../magios/README.md).

Sibling contracts: [ambient-mode.md](ambient-mode.md), [ambient-call-sink.md](ambient-call-sink.md),
[thinking-map-canonical.md](thinking-map-canonical.md), [magican-keyboard.md](magican-keyboard.md),
[monitors.md](monitors.md), [task-verdict.md](task-verdict.md), [muij-graph.md](muij-graph.md),
[today.md](today.md), [voice-call-boundary.md](voice-call-boundary.md),
[app-native-widgets.md](app-native-widgets.md), [execution-panel-delta.md](execution-panel-delta.md),
[task-detail.md](task-detail.md), [plan-inspector.md](plan-inspector.md).

## Identity and build

Unified Work is implemented on `codex/unified-work`. Work replaces the Tasks
tab at the same position and composes the existing full task/internal/monitor,
Attention and Plan Inspector screens. New work keeps the ordinary task form.
Native pushed details add Crew controls and version-checked definition editing,
playbook history/restore and version-checked owner editing (a conflict keeps the
draft), per-focus-area shared documents, guided Quality and advanced policies, exact artifact
review and local captures, Autopilot reviews and Team routines controls. Team routines dispatch one
stage per focus area of each harness agent, read from `GET /api/magician/v2/agents` when the sequence
starts (`WorkOperationsView.teamRoutineTargets`), never a hard-coded list.
All use ThemeManager tokens and themed typography. Frozen connection snapshots,
lifetime invalidation and late-response guards protect reads and confirmations.
Crew definition editing freezes the opened version and scope in its sheet
session, including the first presentation. Editing help opens separately so
accessibility text does not squeeze the editor behind the keyboard.
Filter/page changes during a pending write are coalesced to the latest request
and loaded after it finishes, retaining any write error. Policy confirmations
pin the version shown when opened. Ordinary task creation keeps the draft open
on API/network failure, shows the error and rejects duplicate submissions;
New work from another view returns to the Tasks lane.
Artifact previews stream through the frozen URLSession client and enforce the
existing 128 MiB ceiling on declared and delivered bytes. Oversized responses
stop before EOF even without Content-Length; failure/cancellation stops the
transfer. Accepted previews retain their bytes and review/open/share controls.

The complete migration adds guided Crew creation/editing, model and thinking choices
and global-default clearing, roster leaderboard/hierarchy/pipeline views,
full rules and goal-aware memory inspectors, and eligible isolated-user memory
merge. Playbooks link to real configured members and runtime progress.
Quality adds frozen note/file references, explicit new-work versus source-task
attachment, owner/status filters and independent previous-attempt comparison.
Task/thread and Work deep links retain exact record/run identity; unknown threads
remain read-only, and connection changes invalidate task transports as well as
Work editors.

Related tools open the connected web console through its normal browser login.
No native bearer or Cloudflare secret is placed in a URL. Only pages that consume
member/task/run filters receive them; global tools remain visibly workspace-wide.
The validated web Back to Work link retains the original item. Remote access to
Work and its listed specialist pages is authorized; existing session and API
permissions remain in force. Native Back retains the originating detail stack.

`make test-ios-work` runs Work API/deep-link/theme regressions and Tasks/Attention/Work
XCUITests on the selected simulator. See [Unified Work](../unified-ui/unified-work.md)
and its [qualification record](../unified-ui/unified-work-qualification.md) for
coverage, simulator evidence and specialist/provider limits.

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

Settings → Voice & audio (Talk behavior rows) owns a local **Dictation**, **Hands-free/FluidAudio** or **Live/realtime**
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
  Note links (`/notes?path=…&workspace=…`, the same route on a paired origin, `magican://notes?…`)
  in chat, task summaries, Attention and notes open that note natively (`NoteLinks.swift`); a link
  for a workspace held by exactly one other paired machine switches to it, otherwise it says
  "This note is in another workspace" without loading anything.
- Every other chat link (Markdown replies, structured `text`/`callout`/summary copy, structured
  actions and source rows, activity details) goes through one router, `ChatLinkRouter`
  (`NoteLinks.swift`), over the shared rule set `classifyActivityLink` (`Shared/ChatTurnActivity.swift`,
  the Swift twin of web `classifyRef`). `file://` reads as its absolute path; a path inside a task's
  `outputs/` (absolute, `file://` or `/api/magician/v3/tasks/{id}/outputs/…`) opens in the
  authenticated `ArtifactViewer`, as does any other `/api/…` resource and an absolute link on the
  paired origin; other host paths and workspace-relative files open on the machine (chat-session
  `outputs/open-file`); chat links (`/chat?session=…`, `/t/<thread>[/chat|files|settings|models][?session=…]`,
  `/history`) open that conversation, or the thread's latest one; `/t/<thread>?selected=…`, `/work…`,
  `/tasks…`, `/crew…` go to Work; Attention, Today, Settings, Observe, `/notes` and `/vibe` open their
  screens; only other app routes load in the authenticated web view; http(s)/mailto/tel go to the system.
  A link it cannot open shows "Can't open link". Links are drawn in the accent colour, underlined, and
  paths a reply only mentions (inline code, bare `/…/file.ext` in prose, `file://`) become links
  (`ChatPathLinkifier`).
- A thread's URL slug follows its name; its id never changes. A `/t/<key>` link (slug, old slug or
  id) resolves to the canonical id through `GET /api/magician/v2/ui-threads/{key}` before opening
  (`ThreadDirectory`, cached; the key is used as the id when the lookup fails). Thread chips show the
  thread's name, falling back to its id. New Thread asks for the name first (Create stays off while it
  is empty), posts only the name — the server derives id and slug — and opens the new thread.
  Terminal task cards expose **Publish to Notes** (card and Actions sheet; the task view keeps a top-bar
  action) using scoped server-owned defaults, serialized taps, and in-place success/failure.
  An archived task is terminal too ("Archived", muted, no polling), so a card whose last status update
  never arrived settles on the task store's `archived` instead of reading "Running".
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
  - Mic mode in the call panel is a two-segment control, **Open mic | Hold to talk** (the same words
    on iOS, Android and web); the active segment is accent-filled, and Hold to talk is disabled for a
    translation profile. The mic button's shape follows the mode: Open mic is a round mute toggle whose
    ring follows the mic level, captioned "Listening · tap to mute" / "Muted · tap to unmute"; Hold to
    talk is a wide capsule that reads "Release to send" while held. The status line leads with the
    mode ("Open mic · Listening", "Hold to talk · Ready"), and the composer's live strip shows
    "Live · Open mic / Muted / Hold to talk" with a matching icon (a hand, never a slashed mic, for
    Hold to talk).
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

- **Engine chip:** installed engines come from `GET /plane/engines`; Magician/Pi
  model choices come from `/chat/models`. Each send captures `harness_engine`
  and typed `model_selection`, or the CLI engine's native `harness_model`.
  UserDefaults persist model/thinking; queued/concurrent sends share admission.
  The chip (and VibeDev's coding chip) tags the selected thinking level, also in its
  accessibility label/value, when the model's thinking mode is not `none` (chat:
  Magician/Pi only; `ChatThinkingControl.chipLabel`).
- **Mentions** (`magios/Shared/MentionCatalog.swift`): `@tutor`, `@tutor #quick`, `@brainstorm`,
  `@vibedev`, `@vibedev #discuss`. `insertText` stays `feature:<slug>`; `serialized` is the literal
  command (the ASCII space in `@vibedev #discuss` is load-bearing — `#` is a token character).
  Detection and dispatch are server-side.
- **Slash commands** (`magios/Shared/ComposerCommands.swift`,
  plan,
  engine/model/voice design): `/` as the
  composer's first character opens `/budget`, `/budget default`, `/engine`, `/run-engine`,
  `/voice-mode` in the `@` picker's card, filtered as you type; a command opens its options there
  with a Back row, and sending a composer that holds exactly a command, a partial one whose every
  word is in a single command's name (`/eng`, `/budget defa`), or an open level's prefix
  (`/engine pi son`), opens or keeps the picker; any other text after `/` (`/budget please help`)
  is sent as a message. Send options are hidden while the picker holds the send. Options run
  client-side (never a chat message),
  clear the composer and post a short toast over it (the server's `message`/`error` on failure):
  - `/engine`: a **Now:** note (this device's engine · model, and the server's engine
    everywhere), the scope row, one row per installed engine. Magician/Pi, and an engine with
    more than `default`, open a model level: the `GET /chat/models?engine=` rows (loaded apart from
    the chat's own list) set the engine and `selectedProfile`, so the composer sends that
    `model_selection`; roster engines set `harness_model` (`CLI default` first). A catalog model
    whose thinking control offers a choice opens a third level (`/engine pi <model> `, Back to
    models): Auto then the control's levels, the level in force marked for the model in use, else
    its default. Any other model keeps the current thinking level when it offers it, else its
    default. "Default everywhere" sets engines only (`PUT /plane/chat-engine`); the model stays
    this device's choice. Engine `pi` reads "Coding agent".
  - `/run-engine` mirrors Settings → Agentic flows: a **Now:** note, **Same as Chat**
    (`harness_engine: chat`), then engines. Magician lists the `agentic_decision` models from
    `GET /llm/routing` and saves `PUT /llm/routing/agentic_decision {selection}`, pinning
    `magician` with `PUT /plane/engine` first unless runs resolve to Magician (its engine, or Same
    as Chat with a Magician chat default); the coding agent lists "Default" (`model_selection:
    null`) then its catalog → `PUT /plane/engine {harness_engine: pi, model_selection}`; others →
    `harness_model`. Catalog models open the thinking level as under `/engine`.
  - `/voice-mode`: Live / Dictate / Hands-free, current marked, unavailable ones (the voice-mode
    menu's availability) shown as "Not available here"; sets `AudioSettings.composerVoiceMode`.
  - `/budget` → a running/paused task from this chat's task cards (newest first) then `+$25` /
    `+$100` / `+1M tokens` / `+50 iterations` / `Unlimited` → `POST /tasks/{id}/budget`;
    `/budget default` → marks `GET /budgets/defaults`, harness `$50/$100/$250/Unlimited`, native
    `$5/$15/$50/Unlimited` → `PUT /budgets/defaults`.
- **Structured sidecars:** `ChatMessage.presentation` renders only after V1 validation (scalars
  preserved, UTF-8 bounds, HTTP(S) artifact links, ids/MIME ≤ 160 bytes, sizes non-negative
  signed-32-bit), else canonical content. Actions: `copy_text`, `open_url`, `open_task`,
  `open_artifact`, `send_follow_up`, `invoke_server_action`, via the same validated handler as web.
- **Live steps:** iOS sends a stable `chat_turn_id`, mounts the assistant row before the first token,
  and follows the turn while streaming and while a correlated delegated task is active (lazily
  hydrated for history). Live rows come from the [realtime hub](#realtime-hub): events carrying the
  turn's `chat_turn_id`, or the task / execution of a task card that carries it (the server turn
  sink's mapping). The turn's REST event log backfills on start and on every hub resync; the NDJSON
  `/events/stream` is no longer held open. The Steps disclosure shows immediately: chevron, step
  count (no "Steps" word, so the row never wraps; hidden while the run is active, when the row reads
  only "+N" and the latest 3 settled-step dots, `activityStageDotLimit`), turn usage, then **All** (expanded), **Inspect**
  (opens the run in task detail) and Stop. While the run is active there is no separate spinner:
  the latest dot breathes and the caret wears a thin indeterminate ring in that dot's tint (overlay,
  hit area unchanged); Reduce Motion holds both still. Expanded, the header row's vertical padding is
  6/4 pt (collapsed keeps 7/7); the caret has 5 pt after it. **All** opens the full log. Flat-loop
  actions open a running row on `AgenticActionStarted`, and `AgenticActionExecuted` settles the same
  row by the server's `call_id` (a legacy executed event without one keeps its step-iteration-timestamp
  row); on a timestamp tie a start sorts before its finish. A flat-loop model call is one row keyed by
  `correlation.llm_call_id`: "Thinking…" from `LLMRequestSent`, "Thinking with <model>" from `LLMRouted`,
  settled in place by `LLMResponseReceived`; a server without the id keeps one row per response. The
  chat-side `llm.*` copy of the same call (trace-keyed; `llm.succeeded` names the id) folds into that
  one row, and a typed response never re-settles a row the chat-side settle closed. While
  the turn is live, running LLM and tool rows tick their elapsed time (`ActivityLiveClock`, "12s",
  "1m 5s") until they settle. A tool row reads the event's `detail`
  ("metabase-mcp · execute_sql — SELECT …", split into label and target), or for `<pack>__call_tool`
  "<pack> · <name>" from its args with the first ~80 chars of a sql/query/term field as the target
  (`activityToolDisplay`); never a JSON blob over ~100 chars. Terminal task status triggers a REST reconciliation. Events are deduplicated and timestamp-ordered. **Open complete result** reconstructs
  versioned complete-value, container and UTF-8 fragment units, never presenting the model's bounded
  projection as complete, and shows only a verified content-hash prefix.
- Persisted and realtime messages share one native projector. Task updates coalesce by task. Escalation
  cards stay unresolved until their HITL response succeeds. Planning clarifications keep question
  correlation id, responder task id and execution id separate; free-text questions collect an answer
  rather than submitting a synthetic label. **Keep Trying** calls the continuation endpoint. Historical
  cards without authoritative option actions open their Attention request.
- **History:** Sessions and Threads tabs with Personal/Automated lanes, 15-item server pages per tab and
  lane; search uses the mixed-history endpoint (120-char cap) with an explicit all-scope. A thread tap
  opens its newest usable session and keeps History on Sessions filtered to that thread (`ui_thread_id`,
  both lanes) behind a `#name ×` label; New Session there creates in that thread. `#general` (`is_default_session`) is locked with no archive/delete.
  A thread name stays on one line; the full name is the row tooltip and the first line of the
  long-press menu. Archive, restore, and delete hold that row with "Archiving…" / "Restoring…" /
  "Deleting…" until the server answers (the request waits up to 10 minutes), one change at a time.
  A deleted session's row reads "Deleting…" (inert) until the server answers; one delete runs at a
  time, and the open chat moves to a new session only after the DELETE succeeds (a failure keeps the
  row and shows the history error). While the panel is shown, a session's Working state flipping on
  the 3 s `/chat/activity` poll, or a turn finishing in the open chat, re-reads the current page
  quietly (rows, tab, lane and search stay), at most once per poll cycle.
- **Older history:** a session opens on its newest 200 messages (`has_more` from the session detail;
  a server without it counts a full page as "maybe more"). Scrolling back to the top loads one older
  page (`GET .../messages?before=<oldest id>&limit=50`) per return to the top, never during the open
  (the top row's "Load earlier messages" also loads one on tap, for a transcript that fits on screen);
  the row that was first stays in place, a "Loading earlier messages…" row shows meanwhile and a
  failure offers Retry. Pages for a chat the user left are dropped.
- **Answer route:** `AttentionItem.respondCall` posts the server `respond` handle when present
  ([respond handle](../magician/hitl-attention.md#respond-handle-respond)).
- **Attention** is live-only: Requests, Approvals, Escalations, Failed, All. Counters use backend totals;
  each lane advances its own cursor. Lane chips sit on a UIKit touch layer because SwiftUI button hit
  regions were unreliable on devices. Failed items swipe-to-dismiss. Live updates ride the
  [realtime hub](#realtime-hub) while the view is shown (text and binary frames are equivalent); a hub
  resync refetches the lanes. An open answer sheet closes with
  a brief "Answered elsewhere" notice when its ask is settled on another surface — a realtime
  resolution (`HitlResolved`, `UserRequestResolved`, `*.resolved`/`.expired`/…) names its correlation
  id, or a refetch no longer carries it in any lane — and a submit for such an ask is refused rather
  than posted. This client's own answer, cancel or dismiss never reads as answered elsewhere; a fresh
  `HitlRequested` under the same correlation id makes it answerable again.
- **Asks answered in place** (`ComposerAskDock.swift`): one card, one set of controls (choices,
  confirm/deny, text, plan approval) and the Attention respond path serve the composer ask dock and the
  task detail screen. Taskless questions also match their originating chat session from the request ledger or feed schema.
  Long arguments and choices scroll separately; Review opens the full request. Tool scopes distinguish one action from the whole integration, and MCP/CLI timeline labels name the underlying operation.
  The dock (chat and VibeDev) lists every open ask except a dismiss-only notice (`AttentionItem.isDismissOnly`: a service-health or expired notice, left to Attention), this conversation's first: the session's escalation cards plus feed asks scoped to the
  session/thread — the hitl scope **or the feed row's own `ui_thread_id`** — or owned by a task known to
  belong to the thread (its task cards, its cards' tasks, the tasks of thread-stamped feed rows), so a
  Relentless ask carrying only `task_id` + `ui_thread_id` counts as here. An ask stamped with another
  chat session is never here, and the shared default `general` thread alone never places one here
  (typed text answers only this chat's question); every other ask follows with a
  "From <chat or task> · <agent>" line. Forms, secrets, grants and diffs open
  the focused `AttentionDetailModal` over the current screen (`attentionAskSheet`), which closes when
  the ask settles elsewhere. Options `approve`/`reject` (the Relentless `relentless.accepted_review`
  ask) render as the decision; an ask whose `input_schema.context` (or row) names `review_href` shows
  its `review_label` ("Review draft") and opens the Work Quality destination in-app
  (`HitlReviewLink` → `WorkNavigationTarget`), in the dock, the task screen and the Attention sheet.
  `AppTabView` shares its `AttentionViewModel` as `\.attentionFeed`, so an answer anywhere clears
  everywhere. Visible ask surfaces share live refresh with a 30-second missed-event backstop,
  suspended in the background; failed optimistic answers remain retryable.
  VibeDev's new-run and follow-up composers list every open ask too, run turns' and project-session
  questions first, with native plan review and the same focused answer sheets.
- **HITL modals:** typed controls for all canonical input kinds. `tool_authorization` and
  `sandbox_override` name the subject verbatim in mono with the broken policy and covered roots, refusal
  first, and one button per offered grant (`allow_always` writes the session allowlist). Matches web
  [unified-task-panel.md](../unified-ui/unified-task-panel.md), *Answering an ask*.
- **HITL surfacing** (plan C2–C4): a delegated
  child's ask (`metadata.delegation_child`) shows "from <agent>" and answers with the child's
  `execution_id`; `form` asks answer `{id, value}` / `{id, selected_ids}` / `{id, skipped: true}`;
  a `waiting_without_question` escalation offers Cancel run (`POST /executions/{id}/cancel`) and Open
  task instead of an answer field; `mcp_oauth` opens `input_schema.url` and answers
  `external_action_completed`, and its Attention sheet offers **Not now**
  (`dismiss_label`), which posts `{type:"aborted"}` so the computer cancels the sign-in
  (the composer dock hands sign-ins to Attention). Unknown sources and input types fall back to a free-text answer.
- **Preference alignment holds** (P6.2; `Shared/AlignmentAsk.swift`, `Magios/AlignmentAskView.swift`).
  - A `confirmation` whose `input_schema.alignment` decodes is a hold. A malformed block never fails
    the row; it reads as no hold.
  - iOS is an owner-private surface in the v1 canary. The sheet reads the reason, the cited preferences
    and any wording readback from `GET /hitl/{id}/alignment?surface=ios`.
  - It keeps the act's typed choices, "Change what Magician remembers" (dashed) and a reply field
    apart. Nothing is preselected, and the hold's cancel reads "Cancel the action".
  - A lasting change takes two steps: the new wording first, then "Save this wording" carrying the
    readback's token in `other_value`.
  - A typed reply posts `{type: text}`, which is an instruction, never consent.
  - A combined ask (`alignment.also_covers`) lists the approvals it also settles under "It also
    needs your approval:"; allowing it approves it too.
  - **Your preferences** and **Notes from recent checks** are no longer native screens: they live
    in the embedded web Settings (Settings → Magican settings → Preferences). The request and
    parsing helpers (`OwnerPreferences`, `PreferenceNotice` in `Shared/AlignmentAsk.swift`) and
    their unit tests remain.
    A footnote says how to swipe. A preference stored as `key: {json}` reads as a title
    and its fields (`PresentedStatement`, the web's and Android's rule, fields sorted by
    key). Rows sit on `surfaceColor` over the theme background, as Settings does, never the
    system grouped grey. `OwnerPreferencesRenderTests` draws it in Sorbet light and dark to
    `PREFERENCES_SHOTS_DIR`.
  - Same contract as web [unified-task-panel.md](../unified-ui/unified-task-panel.md) and
    [preference-alignment.md](../magician/preference-alignment.md).

A streamed chat reply is a draft. The SSE `done` frame carries the persisted
reply, which the server has checked against the turn's tool results, and its
`assistant_message.content.text` replaces the streamed text. That final text is
also what Voice reads aloud, so a reply that failed the check is shown and
spoken under its "Not fully verified" caveat.

**Turn state after a stream drop or relaunch.** iOS sends `continue_on_disconnect: true`, so
the server keeps a turn running when the reply stream or the app goes away. `ChatViewModel` reads
the session's latest turn from `turn_state` on `GET /chat/sessions/{id}` (load, relaunch, hub
resync) and on the 2 s `GET /chat/sessions/{id}/queue` poll, and every change from the
`chat.turn.state_changed` agent event (`ChatTurnState`). What the transcript shows:

- `running`: a live assistant bubble keyed by `chat_turn_id`, with its steps backfilled from
  `/turns/{id}/events` and followed live, and Stop in the composer. The turn may have been sent
  from this device, another surface or the queue.
- `completed`: the reply (`ChatMessageReceived`, same `chat_turn_id`) replaces the bubble. If the
  reply was missed, the transcript is re-read.
- `stopped` with `user_stop`: "Stopped". With a watchdog reason (`turn_stalled`,
  `turn_wall_clock_exceeded`), the persisted notice is the reply and gets no line.
- `failed`: "This turn failed before it could answer". With `interrupted`: "Interrupted before it
  finished".
- `turn_state: null` on a load (after a server restart) with a trailing question and no reply: the
  question reads as interrupted. Its own `running` event, if one arrives, makes it live again.

A settled state is never replaced by a stale `running` one. When a server names a newer turn, or
none, iOS stops treating any other turn as running. When a reply stream ends without a reply
(no tokens, an `error` frame, `cancelled` in `done`, a dropped connection), iOS keeps the
bubble and lets the turn state decide. If the stream dropped while offline, the bubble stays
live until the poll answers. A server without `turn_state` gets the old behaviour, plus
"Stopped" or the failed line from the stream's own frames. Realtime voice turns (`voice-*`
ids, or rows with a `presence_session_id`) never get a bubble.

### Tasks

Work retains the native task workspace with three lanes (Tasks / Monitors /
System work), alongside Crew, Playbooks, Quality checks and Team routines.
Work → More → Goals (`Work/WorkGoalsView.swift`, rules in `Work/WorkGoals.swift`) shows the web's goal tree at parity.
All work shows a foldable Goals and Playbooks summary (`Work/WorkLandingSummary.swift`; folding, counts and landing limits in `Work/WorkSummaries.swift`) under the switcher, read with `WorkLanding.goalReadLimits` (main-goal folders, 32 notes) once per visit.

Goals with status `done` or `dropped` are closed. The Goals tree, its counts and the All work summary show only open goals (`workOpenGoalTree`): an open goal under a closed one moves up to its nearest open ancestor, or heads its own tree, and its row reads "Its parent goal “…” is closed." An **Open N | Closed M** switch (remembered; the summary's "See closed goals" link and a `/work?view=goals&goals=closed` or `magican://work?view=goals&goals=closed` link select Closed, a plain Goals link selects Open) shows each workspace's closed goals, newest `closed:` date first, then undated by title, with their parent and a **Reopen** button. Each open goal has **Mark done**. Both save through `PATCH /api/magician/v2/notes/front-matter` with the revision the note was read at: done sets `status: done` and `closed: <today, device time zone>`, reopen sets `status: active` and removes `closed:`. A 409 says the goal changed since the screen loaded and reloads the goals.

- **Regular lane:** presets (All / Inbox / Today / Overdue / Running / Completed) computed server-side
  over the whole pool and badged from `counts` (no badge when unreported); tag chips, search, status
  ledger. Pages are 50 (`limit`/`offset` on `/v3/tasks` and `/v3/tasks/internal`) with a "Load more · x
  of N" footer; a legacy backend without the envelope degrades to one full response. Requests carry
  `view=<lane>` and `today=YYYY-MM-DD`. `TaskFilter.matches` runs locally only where the server has no
  answer: a selected tag (request drops `view=` and retains `today=`), a binary without `counts`, and the lane-change round
  trip. The 5 s completion grace is client-side (`gracedRows`), starting from the tick.
- **Internal lane:** every `/v3/tasks/internal` row (web's Any-status default) with its own status/agent
  filters, search and sort; regular presets and tags hidden. Filter changes reload the server query;
  cross-surface navigation clears old constraints and refreshes the destination's own feed, including
  Internal when its filters were already clear.
- **Cards:** the web state-action matrix, **Result** when web `hasFinalResult` holds (primary on
  completed cards → Output → Result), Reset to Ready only for failed/cancelled tasks that have run, a
  **↻ Recurring** chip (`TaskCronDescription`) instead of a raw cron pill, and an Actions sheet.
  Swipe right = Complete / Mark not done; left = confirmed Cancel/Delete; execution controls are never
  inferred from status for swipes.
- **Partly done:** `completed` with `completion_kind: partial` reads "Partly done" (warning tone) with "Open: <first open item>" (`TaskCompletion`) on task and VibeDev rows, headers and turns.
- **Detail:** Overview, Run, Output, Plan, History tabs, with the nine-state verdict
  ([task-verdict.md](task-verdict.md)). Output: Result, task-scoped **Deliverables**, then a collapsed
  **Intermediate artifacts & evidence** grouped Direct / Delegated / Persisted. Run groups delegated
  children (`run.delegations`) into collapsible envelopes (per-device Grouped | Chronological) and
  streams exact-execution `ExecutionPanelDelta` snapshots, bounded to the newest 200 events. History
  shows `started → ended · duration`. Historical runs are inspect-only; Pause/Resume/Steer/Stop bind to
  the server-authorized active root execution. Reset is a checked server lifecycle transition.
  Controls retain their opening connection and reject actions after a machine or credential switch.
  Ask docks recover older and exact requests, prefer canonical review contracts, and scroll long choice lists; unacknowledged answers stay retryable and closed approvals cannot start a second run.
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
- **Live Activities:** the task Live Activity filters realtime hub events to its `task_id`; a watchdog
  ends a stalled activity, and a hub resync reads the task's stored status once so a terminal event
  lost across a reconnect still ends it. A task-scoped ActivityKit push token keeps it updating while the app sleeps
  (ambient and observation activities stay local-only). Teardown drains an in-flight token
  registration before deleting its route; an unbound or failed dispatch cannot adopt another run's
  events.
- **Magician at work** (`CodingRunsActivity`, `CodingRunsActivityAttributes`, widget
  `CodingRunsLiveActivity`): one summary Live Activity for VibeDev coding runs, VibeDev crews and
  Work tasks, read from `GET /work/live` (`GET /vibedev/runs/live` when an older server answers
  404, until the next foreground). Lock screen: "N running · M needs you", the featured item (kind
  icon, title, project, step), the next two and "+N more". Dynamic Island: the featured state dot,
  the running count or "!" when something needs you, and expanded one button — **Approve**
  (`ApproveRunChangesIntent`, answers the run's diff approval by id without opening the app) or
  Answer / Open (the item's `deep_link`). The type, its `coding_runs` / `coding_runs_start` routes
  and push-to-start are unchanged; the new content fields are optional, so an older server's
  headline and detail still render. `relevanceScore`: Observation and Ambient 100, a summary that
  needs you 80, a followed item 60, a routine summary 40.
- **Follow** (`FollowedItemActivity`, `FollowedItemActivityAttributes`, widget
  `FollowedItemLiveActivity`): a Follow bell on the run screen, the crew card and the task screen
  `POST`s `/work/follow` (a 409 `follow_limit` past three is shown), starts that item's own Live
  Activity and registers its token as a `work_item` route with the item id in `task_id`; the server
  moves it and ends it with the item's final state. Unfollow `DELETE`s the follow, ends the activity
  and removes the route. Each foreground `/work/live` answer refreshes followed activities and ends
  any whose item ended or is no longer followed; an answer asked before the last tap does not change
  the follow list. A machine switch ends them. `magican://work?task=…` opens the task.
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
  `--text-on-accent`. Every tab binds its navigation bar with
  `toolbarBackground`/`toolbarColorScheme` so a live palette flip repaints it; bars of
  background tabs (which the UIKit restyle walk cannot reach while detached) are
  re-styled when their tab becomes selected, and the Themes sheet rebuilds its bar on
  the applied-theme revision because a sheet resolves its toolbar background once per
  presentation.
  Work task-card actions, including Reset to Ready, use that explicit accent
  foreground for text, icons and busy indicators. Filled status actions choose
  their foreground from the status fill instead of inheriting the accent token.
  Anything drawn in `onAccentColor` sits on `accentFillColor`
  (`accentFillPressedColor` when pressed) — primary and `.borderedProminent` buttons, the user's chat
  bubble, selected chips/segments, number badges, send/mic buttons, toasts, the keyboard's prominent
  buttons and callouts. Light themes fill with the accent / accent hover. Dark themes fill with a deeper
  shade of their accent (same HSL hue and saturation, lightness lowered until white reaches 4.5:1;
  pressed about 5.5:1), and their on-accent colour is white. Accent text, strokes, outlines, progress and
  soft tints keep the bright `accentColor`. All theme variants mirror unified-ui's brand, display, body and mono roles (bundled
  Outfit, Manrope, Geist Mono, Bricolage Grotesque, Pixelify Sans, Permanent Marker, Lilita One).
- **Settings** (`SettingsView.swift`) keeps only this iPhone's own settings, as one headed list
  with a search field (`.searchable`, "Search settings"). Every row has a title and keywords
  (`SettingsSearchRow`); a query shows the rows where every word matches, hides empty sections,
  and with no match offers to search Magican settings. Section headings are bold, left-aligned
  titles. A horizontally scrolling chip bar pinned under the search field (one chip per section
  with matches; hidden when only the top row or nothing matches) jumps to a section on tap and
  highlights the section in view as the list scrolls (`SettingsSectionSpy`, choice logic in
  `SettingsSearch.activeSection`). Sections, in order:
  - (top) **Magican settings ›** — the paired machine's shared web Settings, full screen
    (`EmbeddedWebSettingsView.swift`). Its keywords cover what moved there (budgets, harness
    credentials, preferences and check notes, storage maintenance, speaking voices, trust and
    rules, workspaces, models), so searching for one of those finds this row. Unpaired, the row
    says to pair first instead of opening.
  - **Voice & audio** — this iPhone's speaking voice, Talk to Magican (Siri), Talk behavior.
  - **Notifications** — permission, lock-screen questions, At a glance (widget, Live Activities).
  - **Keyboard** — Magican Keyboard settings, Reset learned words.
  - **Connection** — Machines, self-hosted connection (route, QR, endpoint), service health.
  - **Help & about** — How to Use, versions: Magican iOS (marketing version and build), Magician
    backend (`versions.magician`, else `version`), and Magician API, Magicutor and Desktop when
    `/health` reports them. The Connection service rows show the same Magician, Magicutor and Desktop
    versions as badges.
- **Embedded web Settings:** loads `<ui_origin>/settings?embed=1&host=ios`, where `ui_origin` is the
  active machine's entry in `GET /remote-machines` (matched by `api_origin`, else `is_self`) when it
  is reachable from the phone (not loopback; not a private-network address while paired over the
  remote route), else the paired `publicOrigin`. The `WKWebView` uses the persistent default data
  store and adds no device or Cloudflare headers: the page signs in with the ordinary web sign-in
  (`/login?redirectTo=…`), and that sign-in and any Cloudflare Access login survive relaunches.
  Only `/settings`, `/login…`, Cloudflare Access sign-in pages and subframes load inside; other app
  pages (including client-side routes, caught by watching the URL) and external links open in
  Safari. It shows a progress bar and a Retry state for failed or empty pages.

## Networking

Magios routes all intelligence to the Magician backend over the customer-owned mobile origin enrolled
at runtime (`connect.<zone>`, not compiled in). `scripts/ensure-magician-tunnel.sh` configures
**path-based routing**:

- `/api/*` — REST **and** WebSockets — to the selected backend (`localhost:3002` native, or the container
  host port, normally `localhost:13002`). Cloudflare proxies WebSocket upgrades natively.
- `/host/*` — the Tauri Desktop Host Gateway (`localhost:3017`).
- `/health` — the backend's aggregated health (Magican, Magicutor, Tauri reachability) for the Settings
  service ledger, plus component versions (`version` = Magician core, `api_version`, and a `versions`
  object with `magician`, `magician_api`, `magicutor`, `desktop`). Every version field is optional;
  older servers send only `version`, which was the API crate's.
- Everything else — `404`.

`scripts/ensure-connect-access.sh` gates the origin behind Cloudflare Zero Trust Access and writes the
origin, Access audience and issuer to runtime env. Magician Settings issues separate iPhone and Android
five-minute QRs; the exchange is the only path-specific bypass and returns the outer credential plus a
revocable per-device token. Magios probes an authenticated route before committing the atomic Keychain
profile, then attaches both layers via `MagicianAccess`, which derives the device bearer and outer
credentials from one profile snapshot per request (no mixed-authority requests during rotation).
Requests that carry the device id also carry `X-Magician-App-Version: <version> (<build>)` (for
example `0.4.11 (222)`), so web Settings → Devices can show which app build each iPhone runs. Legacy
Cloudflare fields migrate lazily into Keychain; the plaintext is deleted only after exact read-back.

### Multi-machine profiles

An owner can run several Magicians (one tunnel and hostname per machine; plan
2026-10-03-machine-registry-and-picker.md).
Magios keeps one paired profile per machine and switches between them without re-pairing:

- **Store:** Keychain account `mobileConnectionProfilesV2` (same service and shared access group as
  before) holds `{"active_origin", "profiles": {origin: MobileConnectionProfile}}`, keyed by the
  normalized, lowercased origin (`MobileConnectionProfiles` in `magios/Shared/MagicianAccess.swift`).
  Each profile keeps its own device token and Cloudflare credential; nothing moves between machines.
- **Migration:** when V2 is absent, the first read turns an existing `mobileConnectionProfileV1` into
  the only, active V2 profile and persists it. V1 is left in place (read-only) for one release so a
  downgrade still connects; once V2 exists it is authoritative. A corrupt V2 blob reads as "no
  profiles" and never resurrects V1. Forgetting the V1 profile's origin also deletes the V1 copy.
- **Accessors:** `MagicianAccess.connectionProfile` is the ACTIVE profile, so `baseURL`,
  `webSocketBaseURL`, `headers()`, `isMagicianRuntimeURL` and every widget, intent, keyboard, share and
  RealtimeHub call site follow the active machine unchanged. `install(_:)` adds or replaces the profile
  for its origin and makes it active (the exchange's grant-origin == QR-origin check is unchanged).
  The host keeps one pairing per device per machine, so pairing a machine by its other route
  (same-Wi-Fi vs remote) revokes the older profile's token: after verify succeeds, install also
  removes every other stored profile of that machine — same non-nil `machineName` (exchange
  `machine_name`, stored on the profile and optional in V2 JSON) or an origin in the exchange's
  `machine_origins` (normalized like the store keys). Both fields are optional on the wire.
  `activate(origin:)` only switches to an already-paired origin; `forget(origin:)` removes one profile
  and, if it was active, activates another paired one (or none); `clearConnection()` forgets the
  active machine; `forgetAll()` clears every profile. Each change posts
  `magicianMobileConnectionDidChange`, which rebuilds the app tree and RealtimeHub.
- **Settings → Connection (machines):** paired machines first (named by the profile's stored `machineName`, else
  from the registry when its `api_origin` matches, else the host; check = in use; tap to switch;
  swipe or long-press to Forget), then registry machines not paired yet by any route (origin or
  name), read from the active machine's
  `GET /api/magician/v2/remote-machines` (`RemoteMachinesClient`). Pairing one still needs that
  machine's own QR: "Open <name> in Safari" opens its `ui_origin` → Settings → Mobile devices.
- **Push:** only the active machine holds this iPhone's application APNs route. The origin that holds
  it is tracked in the App Group store (`mobilePushApplicationOriginV1`); after a switch, the next
  token registration first `DELETE`s `/devices/me/push?kind=application` on the previous machine with
  that machine's own credential, then registers with the new one. Forget removes the route before
  deleting the credential. Every launch/foreground, and every pairing change, calls
  `registerForRemoteNotifications`, whose token is re-`PUT` (an idempotent upsert), so a route the
  server dropped comes back on the next activation. The registration's `environment` is the signed
  `aps-environment` read from `embedded.mobileprovision` (`development` → `sandbox`), not the build
  configuration: a Release build installed with a development profile holds a sandbox token, and
  calling it `production` made APNs answer `BadDeviceToken` and the server drop the route. No profile
  (App Store/TestFlight) is production; a simulator falls back to Debug → sandbox. Task Live Activity routes are not moved: an activity started on one machine
  keeps its route there until it ends.
- **Answering from a push** (`Shared/AttentionPushAnswers.swift`, `MagiosNotificationService`,
  `Magios/AttentionPushAnswerHandler.swift`): an `attention_requested` push carries no question, only
  `correlation_id`, `source` and — when the backend judged it answerable — `answer_mode`
  (`confirm` | `choice` + `option_count` | `text`); without it the push stays open-only (tap opens
  `deep_link`). The Notification Service Extension reads the ask with the shared credential
  (`GET /feed/attention/{correlation_id}`, bare item or `{item: …}`, 8 s), re-checks it — never
  sensitive, destructive or a multi-question form; confirm = a confirmation; choice = one pick from
  1–4 options, none `requires_input`, count matching `option_count` (an `allow_other` ask stays
  answerable, "Other" is not offered); text = plain `text` — and registers a per-ask category
  `MAGICAN_ASK_<hash>` (merged with the existing set; per-ask categories no delivered notification
  uses are pruned): Approve/Reject (`confirm_label`/`deny_label`), one `opt:<option id>` button per
  option, or a text reply. Every answer button needs an unlocked phone and runs in the background;
  the app posts `/hitl/{id}/respond` with channel `ios_push` (the in-app body: `source`,
  `input_type`, `value`, `execution_id`, planning `task_id`; from `metadata.respond` when
  present) and replaces the notification with
  "Answer sent", "Already answered" (404/409) or "Couldn't send your answer — tap to open", never
  repeating the question. Any fetch failure (404 included — older servers have no single-ask
  read), a mismatch, or the system deadline delivers the push unchanged. The legacy
  `MAGICAN_ATTENTION_APPROVAL` Approve/Reject category (`actions: approve_reject`, channel
  `ios_notification`) still works.
- **Lock-screen privacy:** by default the question and its buttons only appear where iOS hides
  them until unlock. The extension adds them when Settings → Notifications → **Show questions on lock
  screen** (App Group `attentionPushShowQuestionsOnLockScreenV1`, off by default) is on, or when the
  system preview setting is *When Unlocked* (the category's placeholder "Unlock to see the question"
  shows while locked). With previews *Always* or *Never* and the setting off, the push stays
  generic and open-only.
  Settings reads iOS's Show Previews (on appear and on foreground) and, while the toggle is off and
  previews are *Always* or *Never*, explains why questions stay hidden, with an **Open notification
  settings** button (`UIApplication.openNotificationSettingsURLString`).
- **Push-answer diagnostics:** the extension and the app log to `os.Logger` subsystem
  `com.magicbeans100x.magican`, category `push-answer` (Console.app / `log stream --predicate
  'category == "push-answer"'`): correlation-id hash (12 hex), `answer_mode`, privacy decision,
  fetch HTTP status and latency, eligibility result or reason code, whether the category
  registered, and the fallback reason (`privacy`, `fetch_http_404`, `ineligible`, `deadline`, …);
  the app logs the respond status and latency. The last 20 extension decisions are kept in the App
  Group (`attentionPushDecisionsV1`, JSON, no content). Neither ever holds the question, option
  labels or ids, answer values or credentials. There is no in-app diagnostics screen to show them.

### Scope (`principal`/`workspace`) is mandatory on every call

The backend is **fail-closed** on scope: REST reads return empty for the wrong scope, the realtime
WebSocket (`/api/magician/v2/realtime/ws`) is rejected outright (`resolve_required_scope`) and every
event is filtered by principal/workspace (`event_visible_to_scope`). So every REST, SSE, WebSocket and
media call carries authorization from `MagicianAccess`, whose opaque device bearer is bound to
principal/workspace by the server-side pairing record. Scope is never sent in headers, queries or TTS
bodies. Credentials attach only to the exact enrolled Magician origin.

### Realtime hub

`RealtimeHub` (`magios/Magios/RealtimeHub.swift`, plan
2026-10-01-single-event-hub.md) is the app's one
connection to `/api/magician/v2/realtime/ws`. Chat, a chat turn's live steps, Attention, Tasks, Today,
task detail, the Thinking Map, the Tutor overlay and the task Live Activity subscribe to it; none opens
a socket. The voice control socket (`RealtimeVoiceClient`) stays separate.

- One `URLSessionWebSocketTask` with `supports_structured_presentation=true`, opened while anything
  subscribes and closed 10 s after the last subscriber leaves. Reconnects forever with jittered
  backoff (1, 2, 4, 8, 16, then 30 s, each in its upper half), at once when the app becomes active or
  the network path comes back (an open socket is probed with a ping first). Keepalive is the app ping
  `{"type":"ping"}` every 25 s. A connection-profile change reconnects to the new origin and resyncs.
- `subscribe(filter | where: predicate, onEvent:, onResync:)` returns a token; `cancel()` or releasing
  it unsubscribes. `RealtimeHubFilter` covers `eventTypes` (prefix match on `event_type` and on an
  `AgentEvent`'s inner `event.event_type`), `categories`, `taskId`, `executionId`, `chatTurnId`,
  `agentId`, `sessionId` and `userRelevant`, matched against the frame's `refs` / `categories`; a field
  the server left out falls back to the payload. Every event carries the raw frame and, for an
  `AgentEvent`, the unwrapped `inner` / `innerEventType` / `payload`.
- Reconnects resume with `resume_epoch` / `resume_seq` (the highest `seq` seen, else the last
  `hub.hello` seq); frames are deduped by `(epoch, seq)`. Subscribers' `onResync` runs on
  `hub.resync`, on a reconnect whose `hub.hello` says `resumed: false`, and — against a server without
  the hub contract — on a reconnect that brings no `hub.hello` (first event, or 3 s of silence). Each
  owner re-reads its state there with its existing REST refresh. Frames without `seq` are delivered
  undeduped; refs come from the payload.

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
needs a paid team with Push enabled. The app embeds the `MagiosNotificationService` extension
(`<app id>.NotificationService`, App Group + shared keychain group, compiled from only the Shared files
it needs) in every build; it needs no `aps-environment` of its own, so Personal Team Debug builds still
sign. The push lanes' command-line entitlements override reaches it like every other extension, so
`make ios-debug-run-push` installs the app with answerable notifications. `MAGIOS_DEVELOPMENT_TEAM=<team-id>` overrides the team for one
`scripts/magios-run.sh` run. A push-free build keeps widgets and local Live Activities but labels remote
alerts unavailable. A push-enabled launch asks for notification permission
while iOS will still show the prompt. Settings opens on Permissions, which shows
whether notifications are allowed, asks again while the prompt is available, and
opens iOS Settings after a denial.

## Setup

See [magios/README.md](../../../magios/README.md) for checked-in Xcode project build instructions.

## Tests

`TutorNarrationTests` and `TutorInvokeTests` exercise serialized narration and
secure-screen voice admission. Shared speech is resolved inside the main-actor
initializer, and the router's speech callback is explicitly main-actor isolated.
Attention's timer publisher imports Combine directly so its stored type has an
explicit module dependency.

- **`make test-ios`** — the **blocking gate** invoked by root `make test` on macOS (skipped without
  Xcode). Runs `MagiosTests` with coverage and skips `MagiosUITests`. Provisions and checksum-verifies
  the gitignored Vosk framework/model; unit tests isolate HTTP via a mock URL protocol.
  Deferred streaming fixtures drain session cancellation before clearing their handlers.
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

The app icon catalog includes standard, dark and tinted star variants. The
`UILaunchScreen` configuration uses `LaunchStar` and `LaunchBackground`; keep
`Info.plist` and `project.yml` aligned when updating these assets.

Task creation refreshes the current Crew definitions and defaults to the enabled
primary Personal owner. Missing or failed Crew loading cannot silently submit a
fixed owner ID. Crew current-cycle inspection uses the execution panel directly
and keeps execution controls while hiding ordinary task-only actions; bootstrap
and health records and the authenticated web live surface remain reachable.
Program overview reads are limited to four concurrent requests. Work also keeps
paged completed references/manual dependencies, title editing, rechecked thread
moves and separate schedule pause/resume. Schedule edits preserve current limits
and pause state. Task forms use installed theme surfaces/fonts; corpus search,
internal server filters and page sizes remain operable. Confirmed internal
cleanup reports partial failures and excludes App-managed runs; their paged
history, recurring schedule, exact execution/output and originating App remain
inspectable. See the unified Work qualification record for executed lanes and
physical-device/live-provider limits.

Pairing test URLs use `example.com`; deployed remote origins come from the
server and enrollment rather than an operator hostname in source.

## Speaking voices

Settings → Voice & audio contains the This iPhone voice picker; backend
TTS/realtime speaking voices are in Settings → Magican settings (the embedded
web Settings) and in the composer. The composer exposes backend
voices under Replies → Backend host and Live; searchable lists are populated
from `/media/providers`, including FluidAudio/Kokoro. Backend choices patch the
shared scoped media preferences; iPhone voices stay local and are applied to
AVSpeechSynthesizer playback. Realtime and Hands-free changes apply on the next
call. Failed saves retain the previous choice and show an error.
See the [voice selection contract](../magician/realtime-media-rails.md#speaking-voice-selection).

Voice selectors use compact themed current-value rows, including transcription source, reply engine, realtime model, pipeline stages, and personas. A row opens a searchable checked list in the current NavigationStack; composer settings keep one sheet with Back navigation rather than expanding catalogs inside panels or stacking sheets. `VoiceSelectionPicker` / `VoiceSelectionList` share the model picker’s fonts, surfaces, borders, and selection colors.

Dictation and Hands-free pipelines show **Speaking voice** directly below **Speech model**, restricted to that selected TTS binding. Profile order uses the first available configured provider; explicit model choices switch the persona catalog. Only the web Settings (Settings → Magican settings) lists every binding. Transcription providers select recognition models and do not have speaking personas.

Changing the speech model filters the already loaded voice catalog; it does not recreate the voice loader or fetch settings for every selection. Empty advertised voice lists use a neutral provider-default message, with no error/Retry. Cancelled panel requests are not failures; a real load failure offers Retry and clears on successful reload.

God-mode approvals appear in every chat composer in the authenticated workspace,
with the originating chat/run, staged diff and every offered grant choice.
Session grants remain tied to the origin. Answering anywhere clears the shared
request everywhere; enforcement and grant storage stay on the backend.
Exact shell approvals retain session and always choices under the backend's
[invocation scope rules](../magician/god-mode.md).
Scope pickers retain full paths above short duration buttons. Composer review
content scrolls separately; focused God-mode reviews pin decisions above the safe area.

Settings → Services → Service accounts and MCP Attention cards use the shared [browser sign-in](../magician/browser-signin.md) handoff with a native private viewer. Provider registration failures show the server's secret-free explanation. Account rows show the engine's status chip and detail, offer Reconnect when connected, pass the row's `endpoint_alias` to launch, and reload on appear, foreground, pull-to-refresh and viewer dismissal; `unavailable` rows show a grey chip with no Connect. Managed-bot rows (Google Workspace, Telegram, WhatsApp) open `ManagedBotConnectSheet` (in `AttentionDetailModal.swift`), running the `bot/start|input|state|cancel` flow (method, phone, code, two-step password, pairing code, PNG QR, or Google's ticket in the private viewer, with an **Open on computer** fallback), polling every 2 s while signing in. Service choices are full-width themed buttons with 44-point minimum targets.
