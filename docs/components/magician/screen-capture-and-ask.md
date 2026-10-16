# Screen Capture & Ask

On-demand "capture my screen and let me ask about it". Capture is explicit and
user-triggered only: nothing is captured without a chord press or a direct chat
ask. Plan: `docs/archive/plans/2026-06-11-screen-capture-and-ask.md`.

## The flow in one line

Desktop chord (⇧⌥S screenshot / ⇧⌥A region-or-window / ⇧⌥R clip) → the backend
captures pixels first → stages them as chat attachments on today's
`Screens — <date>` session under the `screens` thread → the overlay HUD opens
with the capture(s) as composer chips → the typed (or dictated) ask rides the
normal chat send with `attachment_ids` → the vision-capable chat profile answers
from the pixels → the exchange persists in `#screens`, and a provenance entry
is appended to the `user.screen_observations` memory tier.

## Backend (`magician-api/src/screen_api.rs`)

Thin endpoints under `/api/magician/v2`, deliberately not a media rail: a
capture is one explicit act (the clip recorder's only state is "one recording
may be in flight").

| Endpoint | Behavior |
|---|---|
| `POST /screen/capture` `{mode, session_id?, source_app?, source_window_title?}` | `screenshot`: main display via `/usr/sbin/screencapture -x` (no CuaDriver daemon needed). `region`: the native interactive picker (`-i`; drag = region, spacebar = window-pick); Escape or the 90s picker timeout → `{cancelled: true}`, a cancel, never an error, and callers must open no UI. With `session_id` the capture is staged into that active same-scope chat session; otherwise into the daily `screens` session. `source_app`/`source_window_title` are desktop provenance (frontmost app when the chord fired; the desktop never labels with its own windows): display-only, sanitized (control characters stripped, trimmed, dropped when empty, capped at 200 chars) and echoed so the HUD can label the chip ("screen capture — Safari"). |
| `POST /screen/clip/toggle` | First call: verify the CuaDriver daemon (`cua-driver status` exit code + the idempotent `open -a CuaDriver --args serve`), spawn one `cua-driver mcp` stdio session and call `start_recording {output_dir, record_video: true}` on it (the daemon's SCK recorder → `recording.mp4`). The daemon ties the recorder to the session that started it and tears it down on disconnect (a one-shot `cua-driver call` loses the recording; the ownerless `cua-driver recording start` records no video), so the MCP session stays connected for the clip's life; if the server dies, its stdin closes and the daemon ends the recording. A watchdog stops the recorder at the 30s cap; staging waits for the user's next press. Second call: `stop_recording` on the same session (it replies after finalizing the mp4, named in `last_video_path`; `<dir>/recording.mp4` is the fallback) → ffmpeg samples ≤8 evenly spaced frames (vision models read frames; the mp4 stages as the artifact) → stage everything. |
| `POST /screen/capture/discard` `{session_id, attachment_ids}` | Cleans a capture whose browser-side draft/session ownership changed before send. `ChatService` discards only unreferenced, server-attested screen attachments (≤16 ids; empty `session_id` or ids → 400). It cannot act as a general file-deletion endpoint or race a committed/queued turn. Returns `{discarded_attachment_ids, retained_attachment_ids}` (`retained` = still referenced or not a discardable capture). |
| `POST /screen/desktop-app/launch` `{app}` | Allowlisted Desktop App Copilot launcher (macOS `/usr/bin/open -a` only): Notes / Apple Notes, Calculator, TextEdit, Music. 400 `unsupported_desktop_app` otherwise; 200 `{status: "ok", app}`. Non-macOS → 500 `desktop_app_launch_failed`. |
| `POST /screen/observations/distill` `{days?}` | Desktop connector: clusters `user.screen_observations` by `(purpose, day)`, salience-gates, distils each promotable cluster into an `EvidenceRecord` (`producer = "screen_observation"`), routes a review-gated `user.knowledge` candidate, and appends to user-owned evidence + entity lanes. Default look-back 1 day (min 1); idempotent per `(purpose, day)`. Returns `{observations, clusters, promotable, distilled}`. Generic sibling: `POST /evidence/distill/{producer}` (`meeting` \| `email` \| `calendar` \| future lanes via `evidence::tier_distill::producer_spec`), the same handler the `distill_evidence` tool drives. |

Shared mechanics:

- **Session resolution.** By default find-or-create the `screens` thread's
  active session titled `Screens — <date>`: reuse today's, claim a fresh
  untitled empty session, or rotate via `new_session` for an older day (which
  archives it; the thread keeps history). Callers that own a chat session can
  pass `session_id`; missing, archived or cross-scope ids are rejected.
- **Attachment staging is artifact storage.** Files go through
  `ChatService::store_attachment` into the session outputs dir, the same
  directory the vision prompt-loader reads image bytes from; there is no second
  copy. Vision flows through `TranscriptBlock::ImageFile` →
  `RouterContentBlock::Image` (the profile needs `supports_vision`; flattening
  to "uploaded image X" is only the non-vision fallback).
- **Memory provenance.** One entry per capture (`key = screen:<id>`,
  mode/date/time/session/files) merged into `user.screen_observations` with
  newest-20 retention (the same bounded merge as meeting memory; the tier is in
  the user-tier allowlist in `chat/service.rs`).
- **Observability.** One `screen_capture`-target tracing line per capture.
- Scope comes from the desktop's workspace-bound bearer; callers do not assert
  `anonymous`/`default` on the wire.

Limits are code constants (`SCREEN_CLIP_MAX_SECS = 30`,
`SCREEN_CLIP_MAX_FRAMES = 8`; region picker timeout
`REGION_PICK_TIMEOUT_SECS = 90` in `magician-media/src/media_rails/screen_capture.rs`). `MEET_STT_SILENCE_RMS`
does not apply here.

## Desktop (`desktop/src-tauri/src/screen_ask.rs`)

Three global chords (config `[general]`, default on, `none` disables):
`screen_ask_shortcut` ⇧⌥S, `screen_region_shortcut` ⇧⌥A,
`screen_clip_shortcut` ⇧⌥R. The handlers are stateless; the backend owns all
capture and recording state, so a watchdog auto-stop cannot desync the chord.
Staged results reach the HUD by dual delivery (event `screen-ask-capture` plus
the consume-once pull `take_screen_ask_capture` for a freshly created webview).
On failure the HUD does not open. During a clip, macOS's own menu-bar recording
indicator is the user-facing signal. `trigger_screen_ask()` is the standalone
entry point (for a wake-voice trigger).

## HUD / ChatPanel (unified-ui)

`/hud` binds `<ChatPanel threadId="screens" seedStagedAttachments=…>`;
`seedStagedAttachments` shows server-staged attachments as composer chips once
the active session matches (apply-once; day rollover triggers one scope reload).
Text-bearing attachment sends are optimistic, so the ask and live activity card
appear at once.

The composer stays usable in floating mode; focus and typing do not expand the
transcript (the toolbar icon does). The HUD uses `ChatPanel`'s
`fireAndForget="tutor"` mode, so explicit Personal Tutor and App Copilot prompts
dismiss the HUD right after submit while the draw overlay and chat runtime
continue the turn.

## Agent path (`screen-observation` skill + `/screen/describe`)

A procedure skill (SKILL.md-only; scope copy + `skillshub/` mirror; granted to
Presto) for chat asks like "what does this error say". It captures via `shell`
(`screencapture`, window-scoped cua-driver, or AX-tree-only for named-app text
questions) and understands via **`POST /screen/describe`**: one call captures
(or accepts a provided `image_b64` frame) and answers through the
`screen_understanding` operation → `op-screen-understanding-vision-gpt61sol`
profile (GPT-6.1 Sol, vision). No ad-hoc vision CLIs (`ocr` remains for pure
text extraction). Describe is ephemeral: no thread, no memory; the asking agent
owns the answer.

When CuaDriver is available, describe also inlines the frontmost window's
accessibility tree: the frontmost app's on-screen, current-Space window with the
highest `z_index` (`list_windows` order is not stacking order), read with
`get_window_state {include_screenshot: false, max_elements: 400}`. A driver
refusal (exit 0 with `{code, suggestion}`) is logged and describe answers from
pixels alone.

Hard rules: explicit ask only, announce the capture, clean temp files.
Observation only; acting on the screen stays with mac-operator via delegation.

## Grounding (`POST /screen/ground`)

Pixel-precise click targeting for AX-empty surfaces (canvas apps), used by the
`macos-ui-automation` skill's grounding fallback (AX first, mandatory zoom
refinement, effect verification; plan:
`docs/archive/plans/2026-06-12-screen-grounding.md`). `{target, image_b64}` →
`{found, x, y, confidence, reasoning, image_width, image_height}`; coordinates
are in the input image's pixel space (dims echoed to bind it). Routed via
`screen_grounding` → `op-screen-grounding-vision-gpt61sol` (GPT-6.1 Sol; a swap
is one config line). Accepts PNG or JPEG (cua-driver `zoom` emits JPEG). The
model answers in per-mille normalized coordinates (raw-pixel asking has a
systematic vertical bias) and the server converts. `found: false` is a success
meaning "stop, don't click". Ephemeral like describe.

## Continuous observation (`magician-media/src/media_rails/screen_observe.rs`)

The sustained sibling of the one-shot captures: "watch my screen", not meeting
related (it reuses only the passive meeting listener's lifecycle patterns). ⇧⌥W
toggles a wordless notes-mode session; chat starts condition-bearing watch
sessions ("tell me when the build fails").

- **Loop.** A still every 4 s by default → dHash frame-diff gate (ffmpeg 9×8 gray; an
  unchanged screen means zero model calls and zero retention) → changed frames
  go to the config-routed narrator: the `screen_observation` operation →
  `op-screen-observation-mini-vision` profile (gpt-6-luna, vision) in
  `llm-router.yaml`, reached via the process-global `OperationLlmRouter` → the
  verdicts `nothing` / `note` / `alert`.
- **Record.** Narration streams into a `Watching: <purpose> — <date>` session
  under the **`screen-watch`** thread (separate from `screens`, whose daily
  rotation would archive a running session). Frames are transient; narration
  is the record. Teardown posts a summary via `default_summarizer()` (the
  `meeting_summary` operation → `op-meeting-summary-local`, Ollama Gemma
  on-device, shared with both meeting rails) as `Observation summary` plus one
  `observe:<id>` entry in `user.screen_observations` (newest-20).
- **Modes.** `notes` (never interrupts) or `watch` (`watch_for` → ⚠ ALERT line,
  `stop_on_match` default true). The goal is mutable mid-session
  (`POST /screen/observe/retarget`): ⇧⌥W opens the HUD on the fresh notes
  session as the optional "what should I watch for?" inbox; typing a condition
  there (or in chat) upgrades the running session to watch, `{"mode":"notes"}`
  drops back, and every change posts a 🎯 line. Notes is the default.
- **Lifecycle.** One observation at a time, idle auto-stop (default 15 min
  unchanged screen), `max_minutes` default 120 clamped to 1..=480, repeated capture failure → `Failed`.
- **Deep observation** is an opt-in selector (`deep_observation` on `start` or
  `retarget`) in either mode. After the screen stays stable for 20s it sends one
  high-detail frame to `screen_understanding`, then repeats at most every 5
  minutes while stable; a change resets the timers. In notes sessions a model
  `alert` is downgraded to a note; in watch sessions it follows the normal
  alert flow.
- **Endpoints.** `POST /screen/observe/start|stop|toggle|retarget`,
  `GET /screen/observe/status`. Env knobs are behavioral only:
  `SCREEN_OBSERVE_{CADENCE_SECS, IDLE_STOP_SECS, DIFF_THRESHOLD,
  MIN_NARRATE_SECS}`, plus `SCREEN_OBSERVE_DEEP_DWELL_SECS` and
  `SCREEN_OBSERVE_DEEP_REPEAT_SECS`. Model selection is config, never env.
- **Surfaces.** The **`/observe` page** (top-nav tab next to Meetings): start
  (purpose, optional "Alert me when…", deep-read selector), a live card with
  retarget/deep-read controls, recent sessions, and a nav-tab live dot driven by
  `observeStore` (visible while active). **`/observe/stats`**
  (`ui/unified-ui/src/routes/(app)/observe/stats/+page.svelte`), the
  "Observation pipeline stats" subpage (attention funnel including the
  `screen_observation` source family, channel-assist distill/classify, ambient +
  browser-engine observability). The **⇧⌥W chord**, and **chat** via the
  `screen-observation` skill.
- **Alerts** are delivered in-thread only: status exposes `alert_count`, which
  `/observe` shows on the live card; no tray surface consumes it.

### Client-pushed observation (iOS broadcast)

Host capture is one observation slot; the iOS broadcast extension feeds a
**separate** multi-session registry, so a desktop watch and any number of
mobile observations run together. Frames are pushed, not captured on the host.
Narration lands in the requested `thread` (pass the meeting thread to weave
screen notes into the meeting transcript). Audio is off
(`ObserveAudioConfig::default()`).

- `POST /screen/observe/client/start` `{thread, title?, purpose?, mode?, deep_observation?, cadence_s?, max_minutes?}`: `thread` is required (400 if empty). Defaults: purpose meeting screen-share notes, title `Meeting screen`, mode `notes`; cadence ≥2 s or the host default. Returns `{observe_id, upload_token, thread, status}`; 409 `{error}` on start failure. The client echoes `upload_token` on every frame.
- `POST /screen/observe/frame`: raw JPEG/PNG body (route `PayloadConfig` 4 MiB, same handler cap). Query `observe_id`; echo the token in `x-upload-token`. Newest wins: the loop `take()`s the latest frame on the next tick. **202** `{accepted: true}`; **410** `{error: "observation_gone"}` for unknown session, scope mismatch or bad token alike (no existence oracle, as with meeting audio ingest); 413 over the byte cap; 400 on empty body.

### Audio capture + wake-word correlation (`audio` and surface options)

An observation can also listen. `POST /screen/observe/start` accepts:

- `audio`: `none` (default) / `system` / `mic` / `both`. **System** audio uses
  the Screen Recording grant the frame loop already holds (no new prompt) via
  `ScreenCaptureAudioSource` + `CaptureTarget::DisplayAudio`; **mic** is opt-in
  (separate Microphone grant) via `MicrophoneAudioSource`, the meet-bot's
  capture primitives.
- `audio_profile` and `audio_stage_options`: optional canonical Listening
  overrides. Omitted values use the scoped preference and configured default,
  through the same strict resolver as Meeting Listen/Join. Choices (local,
  FluidAudio, online) come from the backend catalog; this is not the composer's
  browser Web Speech API, which cannot hear server-captured audio.

Two streams, one record:

- **Passive transcription is always on** when audio is captured: finals post
  live (`🔊 System` / `🎙 You`) and feed the teardown summary (whose input splits
  `Screen notes:` from `Heard (audio transcript):`). It only records what was
  heard.
- **Active correlation is wake-word gated.** The vision narrator only sees
  speech addressed to it: an utterance containing a wake phrase
  (`SCREEN_OBSERVE_WAKE_PHRASES`, default `hey magican,magican`). Ambient talk
  never reaches the narrator. An addressed utterance fires an off-cadence
  narration (clamped by `MIN_NARRATE_SECS`); the narrator gets the question and
  current screen and answers in one line, posted as **`💬 Presto`**. The trigger
  watermark is a monotonic `addressed_total` (not the front-evicted vec length,
  which would saturate on a long session).
- Screen-idle auto-stop is off while audio is on; `GET /screen/observe/status`
  carries `audio_source`, `stt` and `transcript_count`.
