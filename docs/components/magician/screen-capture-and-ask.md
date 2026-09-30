# Screen Capture & Ask

On-demand "capture my screen and let me ask about it" — explicit,
user-triggered capture only; nothing is ever captured without a chord press
or a direct chat ask. Plan:
`docs/archive/plans/2026-06-11-screen-capture-and-ask.md`.

## The flow in one line

Desktop chord (⇧⌥S screenshot / ⇧⌥A region-or-window / ⇧⌥R clip) → backend
captures pixels FIRST → stages them as chat attachments on today's
`Screens — <date>` session under the `screens` thread → the overlay HUD
summons with the capture(s) as composer chips → the typed (or dictated) ask
rides the normal chat send with `attachment_ids` → the vision-capable chat
profile answers from the pixels → the exchange persists in `#screens`; a
provenance entry appends to the `user.screen_observations` memory tier.

## Backend (`magician-api/src/screen_api.rs`)

Thin endpoints registered under `/api/magician/v2` — deliberately NOT a
media rail: a capture is a single explicit act (the clip recorder's only
state is "one recording may be in flight").

| Endpoint | Behavior |
|---|---|
| `POST /screen/capture` `{mode, session_id?, source_app?, source_window_title?}` | `screenshot`: main display via `/usr/sbin/screencapture -x` (needs no CuaDriver daemon). `region`: the native interactive picker (`-i`; drag = region, spacebar = window-pick); Escape or the 90s picker timeout → `{cancelled: true}` — a cancel, never an error, and callers must open no UI. When `session_id` is provided, the capture is staged into that active same-scope chat session; otherwise it lands in the daily `screens` session. `source_app`/`source_window_title` are desktop provenance (frontmost app when the chord fired; the desktop refuses to label with its own windows): display-only, sanitized (control characters stripped, trimmed, dropped when empty, capped at 200 chars), echoed in the response so the HUD can label the staged chip ("screen capture — Safari"). |
| `POST /screen/clip/toggle` | First call: verify the CuaDriver DAEMON (`cua-driver status` exit code + the documented idempotent `open -a CuaDriver --args serve`) → spawn one `cua-driver mcp` stdio session and call `start_recording {output_dir, record_video: true}` on it (the daemon's SCK recorder → `recording.mp4`). The daemon ties the recorder to the session that started it and tears it down on disconnect (a one-shot `cua-driver call` loses the recording; the ownerless `cua-driver recording start` records no video), so the MCP session stays connected for the clip's life; if the server dies, its stdin closes and the daemon ends the recording. A watchdog stops the RECORDER at the 30s cap — staging waits for the user's next press. Second call: `stop_recording` on the same session (it replies after finalizing the mp4, naming it in `last_video_path`; `<dir>/recording.mp4` is the fallback) → ffmpeg-sample ≤8 evenly spaced frames (vision models read frames; the mp4 stages as the artifact) → stage everything. |
| `POST /screen/capture/discard` `{session_id, attachment_ids}` | Cleans a capture whose browser-side draft/session ownership changed before send. `ChatService` discards only unreferenced, server-attested screen attachments (≤16 ids; empty `session_id` or ids → 400). Cannot act as a general file-deletion endpoint or race a committed/queued turn. Returns `{discarded_attachment_ids, retained_attachment_ids}` (`retained` = still referenced or not a discardable screen capture). |
| `POST /screen/desktop-app/launch` `{app}` | Allowlisted Desktop App Copilot launcher (macOS `/usr/bin/open -a` only): Notes / Apple Notes, Calculator, TextEdit, Music. 400 `unsupported_desktop_app` otherwise; 200 `{status: "ok", app}`. Non-macOS → 500 `desktop_app_launch_failed`. |
| `POST /screen/observations/distill` `{days?}` | WEG Phase 4 desktop connector: clusters `user.screen_observations` by `(purpose, day)`, salience-gates, distils each promotable cluster into an `EvidenceRecord` (`producer = "screen_observation"`), routes a review-gated `user.knowledge` candidate, and appends to user-owned evidence + entity lanes. Default look-back 1 day (min 1); idempotent per `(purpose, day)`. Returns `{observations, clusters, promotable, distilled}`. Generic sibling: `POST /evidence/distill/{producer}` (`meeting` \| `email` \| `calendar` \| future lanes via `evidence::tier_distill::producer_spec`) — same handler the `distill_evidence` tool drives. |

Shared mechanics (both endpoints):

- **Session resolution** — by default find-or-create the `screens` thread's
  active session titled `Screens — <date>`: reuse if today's; claim a
  fresh untitled empty session; rotate via `new_session` for an older day
  (archives it — the thread keeps history). Mirrors the meet-bot's dated
  threads. Callers that already own a chat session can pass `session_id` to
  stage the attachment there; the backend rejects missing, archived, or
  cross-scope session ids.
- **Attachment staging IS artifact storage** — files go through
  `ChatService::store_attachment` into the session outputs dir, the same
  directory the vision prompt-loader reads image bytes from. No second
  copy. The ask is a normal message with `attachment_ids`; vision flows
  through the existing `TranscriptBlock::ImageFile` →
  `RouterContentBlock::Image` path (profile must have
  `supports_vision`; the flatten to "uploaded image X" is only the
  non-vision fallback).
- **Memory provenance** — one entry per capture (`key = screen:<id>`,
  mode/date/time/session/files) merged into `user.screen_observations`
  with newest-20 retention (same bounded merge as meeting memory; tier is
  in the user-tier allowlist in `chat/service.rs`).
- **Observability** — a `screen_capture`-target tracing line per capture.
- Scope comes from the desktop's workspace-bound bearer like sibling APIs;
  callers do not assert `anonymous`/`default` on the wire.

Env knobs: `MEET_STT_SILENCE_RMS` does not apply here; clip cap and frame
budget are code constants (`SCREEN_CLIP_MAX_SECS = 30`,
`SCREEN_CLIP_MAX_FRAMES = 8`); region picker timeout 90s.

## Desktop (`desktop/src-tauri/src/screen_ask.rs`)

Three global chords (config `[general]`, default ON, `none` disables):
`screen_ask_shortcut` ⇧⌥S, `screen_region_shortcut` ⇧⌥A,
`screen_clip_shortcut` ⇧⌥R. The chord handlers are stateless — the backend
owns all capture/recording state, so a watchdog auto-stop can never desync
the chord. Staged results reach the HUD via dual delivery (event
`screen-ask-capture` + consume-once pull `take_screen_ask_capture` for a
freshly-created webview). On failure the HUD does NOT open. During a clip,
macOS's own menu-bar recording indicator is the user-facing signal.
`trigger_screen_ask()` is the standalone entry point (for a wake-voice
trigger).

## HUD / ChatPanel (unified-ui)

`/hud` binds `<ChatPanel threadId="screens" seedStagedAttachments=…>`;
`seedStagedAttachments` surfaces server-staged attachments as composer
chips once the panel's active session matches (apply-once; day-rollover
triggers one scope reload). Text-bearing attachment sends are optimistic, so the
submitted ask and live per-turn activity card appear immediately even though the
turn includes staged screenshot/clip attachments.

The HUD keeps the composer usable in floating mode. Focus and typing do not
expand the transcript; the user can switch to the expanded transcript with the
toolbar icon. The HUD opts into `ChatPanel`'s `fireAndForget="tutor"` mode, so
explicit Personal Tutor and App Copilot prompts dismiss the HUD right after
submit while the draw overlay and chat runtime continue the turn.

## Agent path (`screen-observation` skill + `/screen/describe`)

Procedure skill (SKILL.md-only; scope copy + `skillshub/` mirror; granted
to Presto): for chat asks like "what does this error say" — capture via
`shell` (`screencapture` / window-scoped cua-driver / AX-tree-only for
named-app text questions), understand via **`POST /screen/describe`**:
one call captures (or accepts a provided `image_b64` frame) and answers
through the `screen_understanding` operation →
`op-screen-understanding-vision-gpt61sol` profile (GPT-6.1 Sol, vision) — the same
config-managed engine as everything else; no ad-hoc vision CLIs (`ocr`
remains an alternative for pure text extraction). Describe is ephemeral
by design: no thread, no memory — the asking agent owns the answer.
When CuaDriver is available, describe also inlines the frontmost window's
accessibility tree into the prompt: the frontmost app's on-screen,
current-Space window with the highest `z_index` (`list_windows` order is not
stacking order), read with `get_window_state {include_screenshot: false,
max_elements: 400}` — tree only, since describe already sends its own frame.
A driver refusal (exit 0 with `{code, suggestion}`) is logged and
describe answers from pixels alone.
Hard rules: explicit ask only, announce the capture, clean temp files.
Observation only — ACTING on the screen stays with mac-operator via
delegation.

## Grounding (`POST /screen/ground`)

Pixel-precise click targeting for AX-empty surfaces (canvas apps), used
by the `macos-ui-automation` skill's grounding-fallback recipe (AX-first
rule, mandatory zoom refinement, effect verification — plan:
`docs/archive/plans/2026-06-12-screen-grounding.md`). `{target, image_b64}` →
`{found, x, y, confidence, reasoning, image_width, image_height}`;
coordinates are in the INPUT image's own pixel space (dims echoed to
bind it). Routed via `screen_grounding` → `op-screen-grounding-vision-gpt61sol`
(GPT-6.1 Sol; swap = one config line). Accepts PNG or JPEG
(cua-driver `zoom` emits JPEG). The model answers in PER-MILLE normalized
coordinates — raw-pixel asking carries a systematic vertical bias — and the
server converts.
`found: false` is a success response meaning "stop, don't click."
Ephemeral like describe: no thread, no memory.

## Continuous observation (`magician-media/src/media_rails/screen_observe.rs`)

The sustained sibling of the one-shot captures — "watch my screen", NOT
meeting-related (it reuses the passive meeting listener's lifecycle
*patterns* only). ⇧⌥W toggles a wordless notes-mode session; chat starts
condition-bearing watch sessions ("tell me when the build fails").

- Loop: a still every ~4s → dHash frame-diff gate (ffmpeg 9×8 gray; an
  unchanged screen = zero model calls, zero retention) → changed frames to
  the **config-routed narrator**: the `screen_observation` operation →
  `op-screen-observation-mini-vision` profile (gpt-6-luna, vision) in
  `llm-router.yaml` — model choice lives in config, no env vars
  (reached via the process-global `OperationLlmRouter` handle set at
  startup) → verdicts `nothing` / `note` / `alert`.
- Narration streams into a `Watching: <purpose> — <date>` session under
  the **`screen-watch`** thread (separate from `screens`: the one-shot
  path's daily rotation would archive a running observation's session).
  Frames are TRANSIENT; narration is the record. Teardown: summary via
  `default_summarizer()` — the `meeting_summary` operation →
  `op-meeting-summary-local` profile (Ollama Gemma, on-device; shared with
  both meeting rails) — posted as `Observation summary` + one
  `observe:<id>` entry in `user.screen_observations` (newest-20).
- Modes: `notes` (never interrupts) / `watch` (`watch_for` → ⚠ ALERT line,
  `stop_on_match` default true) — and the goal is **mutable mid-session**
  (`POST /screen/observe/retarget`): ⇧⌥W opens the HUD on the fresh
  notes session as the optional "what should I watch for?" inbox; typing
  a condition there (or in chat) upgrades the RUNNING session to watch
  mode, `{"mode":"notes"}` drops back; every change posts a 🎯 line.
  Notes is the default by inaction. Lifecycle: ONE observation at a time,
  idle auto-stop (15min unchanged screen), max-minutes clamp (≤480),
  repeated capture failure → `Failed`.
- Deep observation is a separate opt-in selector (`deep_observation` on
  `start` or `retarget`), not a separate mode. It can run in notes or watch:
  after the same screen remains stable for 20s, it sends one high-detail frame
  to the `screen_understanding` operation for a deeper read, then repeats at
  most every 5 minutes while the same screen remains stable. A screen change
  resets the dwell/repeat timers. In notes sessions, a model `alert` verdict is
  downgraded to a note; in watch sessions it can trigger the normal alert flow.
- Endpoints: `POST /screen/observe/start|stop|toggle|retarget`,
  `GET /screen/observe/status`. Behavioral env knobs only:
  `SCREEN_OBSERVE_{CADENCE_SECS, IDLE_STOP_SECS, DIFF_THRESHOLD,
  MIN_NARRATE_SECS}`, plus `SCREEN_OBSERVE_DEEP_DWELL_SECS` and
  `SCREEN_OBSERVE_DEEP_REPEAT_SECS` for the deep-read schedule — model
  selection is config (operations + profiles), never env.
- Surfaces: the **`/observe` page** (top-nav tab next to Meetings) —
  start (purpose + optional "Alert me when…" + deep-read selector), live active
  card with mid-session retarget/deep-read controls, recent sessions, and a
  nav-tab live dot driven by `observeStore` (the rail's "visible while active"
  privacy posture); **`/observe/stats`** (`ui/unified-ui/src/routes/(app)/observe/stats/+page.svelte`)
  — "Observation pipeline stats" subpage linked from `/observe` (attention
  funnel including the `screen_observation` source family, channel-assist
  distill/classify, ambient + browser-engine observability); the **⇧⌥W chord**
  (wordless toggle, HUD as the optional condition inbox); and **chat** via
  the `screen-observation` skill.
- Alerts are delivered in-thread only: status exposes `alert_count` and
  `/observe` chips it on the live card; no tray surface consumes it.

### Client-pushed observation (iOS broadcast)

Host capture is one observation slot; the iOS broadcast extension feeds a
**separate** multi-session registry so a desktop host watch and any number of
mobile client observations run together (no cross-contention). Frames are
PUSHED, not captured on the host. Narration lands in the requested `thread`
(pass the meeting thread to weave screen notes into the meeting transcript).
Audio is off (`ObserveAudioConfig::default()`).

- `POST /screen/observe/client/start` `{thread, title?, purpose?, mode?, deep_observation?, cadence_s?, max_minutes?}` — `thread` is required (400 if empty). Default purpose is meeting screen-share notes; default title `Meeting screen`; default mode `notes`; cadence ≥2 s or the host default. Returns `{observe_id, upload_token, thread, status}`. 409 `{error}` on start failure. The client echoes `upload_token` on every frame.
- `POST /screen/observe/frame` — raw JPEG/PNG body (route `PayloadConfig` 4 MiB, same handler cap). Query `observe_id`; echo the start token in `x-upload-token`. Newest-wins: the loop `take()`s the latest frame on the next cadence tick. **202** `{accepted: true}`; **410** `{error: "observation_gone"}` uniformly for unknown session, scope mismatch, or bad token (no existence oracle — same as meeting audio ingest); 413 when over the byte cap; 400 on empty body.

### Audio capture + wake-word correlation (`audio` and surface options)

An observation can ALSO listen. `POST /screen/observe/start` accepts:

- `audio`: `none` (default) / `system` / `mic` / `both`. **System** audio rides
  the Screen-Recording grant the frame loop already holds (no new prompt) via
  `ScreenCaptureAudioSource` + `CaptureTarget::DisplayAudio`; **mic** is opt-in
  (separate Microphone grant) via `MicrophoneAudioSource` — the meet-bot's
  capture primitives, reused.
- `audio_profile` and `audio_stage_options`: optional canonical Listening
  overrides. Omitted values use the scoped preference and configured default,
  through the same strict resolver as Meeting Listen/Join. Available local,
  FluidAudio, and online choices come from the backend catalog; this is not the
  composer's browser Web Speech API, which cannot hear server-captured audio.

Two streams, one record:

- **Passive transcription is always on** when audio is captured — finals post
  live (`🔊 System` / `🎙 You`) and fold into the teardown summary (whose input
  splits `Screen notes:` from `Heard (audio transcript):`). Zero hallucination
  risk: it only records what was heard.
- **Active correlation is WAKE-WORD GATED.** The vision narrator only sees speech
  the user addressed to it — an utterance containing a wake phrase ("hey magican
  …", `SCREEN_OBSERVE_WAKE_PHRASES`, default `hey magican,magican`). Ambient talk
  never reaches the narrator (no invented screen↔chatter links). An addressed
  utterance fires an off-cadence narration (clamped by `MIN_NARRATE_SECS`); the
  narrator gets the question + current screen and answers in one line, posted as
  **`💬 Presto`**. Trigger watermark is a monotonic `addressed_total` (not the
  front-evicted vec length, which would saturate on a long session).
- Screen-idle auto-stop is disabled while audio is on (a static-screen call
  isn't killed early); `GET /screen/observe/status` carries `audio_source`,
  `stt`, and `transcript_count`.
