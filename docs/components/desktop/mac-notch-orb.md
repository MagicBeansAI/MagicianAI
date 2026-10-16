# Mac Notch Orb

Magician Desktop owns an ambient voice surface that rests as a hardware side
notch on the right screen edge, under the menu bar, and makes a center-screen
arrival before settling back there. It is never painted as an application
window: the native panel is borderless and transparent. The docked ear is an
opaque bezel (rounded on the left, flush on the right) holding the Orb and,
when Attention is waiting, an amber count plus the question.

## Product contract

The orb is a truthful projection of the process-owned voice lifecycle.
`Armed`/`Disarming` are internal names; product copy says **Ready**,
**Resting**, **Rest** and **Wake**. Resting does not claim all microphone work
is off: with wake phrases enabled, the on-device detector may stay ready.

| Phase | Meaning | Palette |
|---|---|---|
| Armed | Wake detection is ready; conversation capture is not active | Armed ember |
| Heard / connecting | A wake was accepted and the voice session is opening | Violet surge |
| Listening | The native conversation input stream is active | Calm aurora |
| Thinking | Speech ended or muted assistant output is being prepared | Amber |
| Speaking | Audible assistant PCM is being produced locally | Teal |
| Voice busy | Another native voice capture owns the microphone; wake is suspended | Graphite |
| Paused / disarming / ended | No ambient capture is claimed | Graphite |

Colors mirror `magios/Shared/AmbientOrbAppearance.swift`. Motion cannot select
a phase: `orb_state.rs` is the authority, and the `/orb` webview only renders
its snapshots and events.

### How a conversation starts

With `orb.enabled`, startup enters **Armed** and shows the side pill. Wake
listening is off unless `orb.wake_enabled`. The default is **holding Left
Option** (double-tap opens Quick Automate): the microphone is open only while
held, and release commits the turn. The microphone closes between holds; a Live
socket stays connected so the next hold skips setup.

When the visible Orb is off but `orb.wake_enabled` is true, only the on-device
Vosk spotter runs, and a valid phrase cold-starts the Orb. It reads the
microphone to recognize the phrase but keeps no audio, submits no speech and
opens no media session.

A conversation starts only from a finalized utterance beginning with the wake
phrase, an explicit **Talk** from the radial actions, or a Left Option hold.
Partial recognition, startup microphone settling, mid-sentence mentions,
mounting `/orb` and expanding or collapsing cannot start capture.

## Native lifecycle

`desktop/src-tauri/src/orb_state.rs` is an injected-clock lifecycle with a
bounded listening leash, cooldown, pause latch and total disarm; as on iOS, an
expired cap lets an in-flight wake exchange finish before disarming. Events:

- `orb://phase` — authoritative state, phase, palette, status, known deadlines
- `orb://caption` — user or assistant captions with an explicit `speaker_name`
- `orb://caption-clear` — removes the matching unfinished caption, never
  finalized history
- `orb://audio-level` — input or audible output RMS envelopes
- `orb://ended` — typed terminal reason and human message

The webview subscribes before rehydrating; both snapshots carry monotonic
revisions so a late getter cannot overwrite a newer event. The compact status
pill and the terminal farewell are mutually exclusive: during the exit window
the farewell is the single visual and `aria-live` owner. User-directed rest
shows **Resting**.

### Window and placement

`orb_window.rs` projects state into a `tauri-nspanel` non-activating `NSPanel`
at status-bar level: all Spaces, beside full-screen apps, outside the app
window cycle.

- **Keyboard focus:** passive updates (inbox refresh, lifecycle, drawer
  resizing, startup) order the panel with the existing NSPanel's
  `order_front_regardless` on the main thread, without making it key, so the
  station stays visible while the user types elsewhere. Tauri/Tao `show()`
  makes the window key and must not be used for passive refreshes. Explicit
  summons request focus, and clicking an input in the notch can take focus.
- **Compact station:** two 40×64pt side notches on the right edge (real notch
  depth is measured so menu titles and status icons stay clickable): the Orb
  ear and, below it, the Magican tray mark. The ears overlap
  (`TRAY_NOTCH_GAP_PT` is −22pt) so they read as one station; only each body
  takes clicks. The pair can sit anywhere along the edge. Hover does not widen
  the ears.
- **Attention count:** the tray ear paints the canonical web badge count of
  waiting Ask-user and Attention items (durable notices, stalled runs,
  failures), from server totals, never capped by a loaded page.
- **Runs ear:** below the tray ear, one dot per work item at work,
  most urgent first, in 28pt rows with a 12pt dot (amber needs you or paused,
  pulsing blue working, green done, red failed; a stopped item gets no dot). A
  dot is a work item: a coding run, a crew (one dot for all its lanes) or a
  Work task. It grows to four rows, then scrolls. The overlay publishes the
  items with `publish_notch_runs` from `GET /work/live` (`NotchRunDot`: item
  id, state, title, optional `path` = the item's `web_path`); the window grows with the
  rows (`runs_ear_height_pt`). Pointing at a dot slides out a label (state,
  run title, up to three lines) via `notch_set_run_label`, which widens the
  closed station to 300pt. The station never becomes key, and macOS sends a
  web view mouse moves only while its app is active, so hovering from another
  app fired no pointer events; while the ear has dots a native poll
  (`ensure_notch_pointer_poll`, every 60 ms) sends the cursor's position over
  the station as `notch://pointer` and the page hit-tests the dots itself. The
  position is the page view's own (`cursor_over_notch_page`: AppKit converts
  the screen point into the pinned page), since the page is wider than the
  window while a label is open; the page hides a label only after three
  readings off the ear and not in the first 400 ms after it opened. Tapping a dot (`notch_show_runs { itemId }`) opens the
  unified-UI `/coding-runs` work list in the side panel: a coding run as
  `?run=<run id>`, highlighted, to steer, stop (after a two-second undo) or
  open; a crew or Work task as `?item=<item id>`, highlighted, with Open (its
  `web_path` in the main window via `open_application_path`) and Follow. Tapping
  another dot moves the highlight (`notch://runs-select`, `{runId, itemId}`); tapping the
  highlighted dot, ✕ or Escape closes it, and the tray ear switches back to
  Attention. A done dot stays green for two minutes, at most the latest four
  (`NOTCH_DONE_DOT_MS`, `NOTCH_DONE_DOT_LIMIT` in `notchRuns.ts`); a red dot
  stays as long as the server lists the failed item. Attention no longer lists
  coding runs.
- **Attention station:** tapping the tray ear opens a 440×560pt station with
  the full unified-UI Attention workspace in a docked auxiliary webview, even
  when nothing is waiting. The global prompt host owns all response types,
  validation and submission as on the web; the retired `SideNotchInbox`
  renderer is removed. The inbox occupies 400×532pt, leaving the ears exposed,
  with search, categories, server pagination and item actions. It connects
  through the origin-bound credential broker, not iframe credential forwarding.
  Closing hides the persistent webview; refreshes never reopen a manually
  closed drawer unless a new item arrives, and resolving the last question
  closes the drawer but keeps the tray ear. Review destinations open the full
  browser workspace through the app router while the notch inbox stays put.
  Voice summon hides the inbox and returning restores it. A debug build loads the panel from its devUrl, which
  Tauri treats as local, so `capabilities/attention-panel-local.json` grants
  the Attention windows close and `open_application_path`; a remote UI gets
  both from the per-session capability.
- **Idle station:** both ears stay available when nothing is waiting.
  Inactivity, the listening cap, a voice failure or battery policy can stop
  listening without removing the station, and keeping the dock visible does
  not rearm capture or extend its keep-awake lease. Terminal settling is fenced
  to the lifecycle revision and presentation generation so an old callback
  cannot override a new summon.
- **Tray without the Orb:** **Hide Orb** hides only the voice control; the tray
  ear stays. **Show Orb** restores it. These change visibility for the session
  only, without touching listening or the saved preference; a summon, Talk or
  Option hold reveals the control. **Rest / Wake** and `orb.enabled` own
  listening and launch visibility. The tray uses the web star mascot
  (`static/brand/star-dark.png`). Setup hides every surface.
- **Startup:** each launch opens the large Orb at center with the configured
  shortcuts (overlay-key hold and double-tap, Orb chord, wake phrase if on),
  then settles to the station. Pending onboarding keeps these surfaces hidden;
  an explicit hold or summon brings capability setup forward.
- **Center:** the large Orb is 620×500 at screen center; there is no detached
  pill and no top-center panel. Double-click the ear to open the center;
  double-click the center, **Settle**, or Escape (after closing an open
  drawer) returns to the ear; a double-click leaves the transcript strip
  closed. A single click on the large Orb toggles the
  radial actions. The Orb sits 151 px from the left so the whole ring,
  including **Home**, stays inside the window, and the conversation cloud
  starts at 272 px; `orbComponentContract.test.js` derives both bounds from the
  source. The global shortcut shows the ear when hidden and returns to the ear
  from the center; it does not grow the ear.
- **Transcript strip:** a single click on the Orb ear, or a Left Option hold,
  the wake phrase or Talk while docked, opens a 360×128pt strip beside the Orb ear with
  about four scrolling lines of the conversation. It is not the Ask-user
  drawer. Both drawers spring open and closed at the same pace; each frame
  changes window origin and size together on the main thread, and the page
  stays at least the open size, pinned top-right. Closing slides the panel off
  the edge with the ears on their own layer in front, then the window returns
  to the ears in one step, so the station does not flash. A strip opened by a
  conversation closes after five quiet seconds; one opened by a tap stays until
  tapped closed. The large center Orb never resizes into a drawer.
- **Drag:** only the docked station moves. Either ear is a handle, and a drop
  snaps flush to the right edge at the dropped height. **Home** on the action
  wheel returns to the top of the edge and clears the saved height, which
  persists in device-local config; center and settling motion never overwrite
  it.
- **Focus:** after `tauri-nspanel` converts the window, focusability is owned
  by the panel's `canBecomeKeyWindow`, non-activating style and cursor gating.
  Do not use Tauri's focusability setter here — Tao implements it via a private
  ivar on its original window class. Direct AppKit use is limited to
  `NSScreen.safeAreaInsets` and the two auxiliary top-area rects. The built-in
  display is preferred; following the active external display is out of scope.

## Voice and lock behavior

The orb reuses Desktop's native PCM capture/playback and voice transports in
`voice_note.rs`. **Settings → Ambient Orb → Conversation Mode** chooses
**Dictation**, the backend's `hands_free` FluidAudio pipeline, or the
configured `realtime` provider, independently of other voice surfaces. Legacy
`recording`/`dictate` normalize to `dictation`; invalid values fail closed to
`hands_free`.

**Seeding:** the first authoritative backend media preference seeds this
device-local field, then `voice_mode_seeded` stops later shared changes from
overwriting it. The startup GET may return compiled defaults, so Desktop waits
for the realtime preference stream and re-fetches behind that barrier; only
that snapshot, an explicit save or a later preference event may seed. Choosing
a mode in Settings marks the seed complete.

**Admission:** a conversation snapshots the mode once, so a racing Settings
edit applies only to the next conversation.

### Dictation

An iterative turn loop: record one utterance, detect its boundary, submit
through the canonical voice-note STT/agent path, render captions, synthesize
through the backend TTS profile, and open a fresh capture. Speech ends after
**1.1 s** of trailing silence or a **45 s** cap; a capture with no speech ends
at `follow_up_seconds`; network processing has a **3-minute** ceiling. The loop
is explicit, not recursive, so stack depth is constant and there is one
microphone owner. All synthesized segments of an answer share one native
prebuffer and drain boundary.

Recorded-turn 422s `unsupported_language`, `empty_transcript` and `no_speech`
show (and speak, if output is on) the backend guidance and open the next
capture in the same chat session. Unknown codes, transport and infrastructure
errors end the conversation; there is no unbounded retry.

### Hands-free and Live

Both use the native conversation WebSocket. Hands-free prepares one upcoming
TTS segment while current PCM streams.

- **Half-duplex Hands-free:** macOS CPAL capture has no echo cancellation, so
  Desktop withholds microphone frames while playback sounds and for a
  **400 ms** tail; the backend enforces the same boundary before STT. Live
  stays full-duplex with barge-in; muted Hands-free output does not gate the
  microphone.
- **Quiet deadline:** `follow_up_seconds` starts when the session is ready, is
  cancelled by user speech or assistant output, and re-arms only after a
  response or a textless/rejected turn. Microphone noise never refreshes it.
  On expiry capture and socket close, the wake detector resumes and the
  cooldown settles to Armed.
- **Turn boundary:** a Left-Option Live session omits the `turn_boundary`
  override, so the backend's manual push-to-talk boundary commits on release;
  the next hold sends `ptt.engage` on the same session. Wake-word and Talk Now
  sessions use `server_vad`.
- **Live readiness:** microphone frames wait in a bounded pre-ready queue until
  the provider websocket consumes (OpenAI handshake 8 s; control-actor
  readiness fence 10 s). The first wake sends only input engagement and cannot
  cancel a nonexistent response. Barge-in is response-aware:
  `response.created` opens one interrupt permit, duplicates coalesce,
  `response.done` closes it; a stale "no active response" ack is a benign
  race. Configuration and resume replay use the actor-ordered non-blocking
  dispatcher so readiness cannot deadlock behind a full channel.
- **Failover:** a fatal Live failure triggers exactly one sequence-checked
  failover to Hands-free via a fresh scheduler task. A Hands-free failure is
  terminal.

### Playback

Playback accepts provider PCM16 WAV and the Float32 WAV from Apple's
`AVSpeechSynthesizer.write` (converted onto the mono PCM16 rail), keeping
provider/model identity in diagnostics. A trailing partial frame is trimmed,
and a streamed WAV with the `0xFFFFFFFF` unknown-length sentinel is
canonicalized to its bounded body. Oversized or truncated declarations,
unsupported formats and wider corruption are terminal errors naming the
provider/model.

Hands-free and Dictation share a jitter buffer: **180 ms** pre-roll,
backpressure by queued audio duration, explicit release of short tails, and a
drain before teardown. Hands-free keeps a **300 ms** lead. Underrun and
dropped-sample telemetry is logged per segment.

### Captions and speaker identity

A VAD boundary with no text is normal: the backend emits
`transcript.user.cleared`, Desktop removes only the unfinished user caption,
and the Orb returns to Listening. `transcript.user.ignored` is reason-aware:
self-echo is silent, a prefix-only turn prompts to continue, a missing prefix
shows the backend-advertised activation phrase; unknown reasons stay visibly
fail-safe, and older backends' cleanup reasons stay silent.

The backend resolves the primary agent at admission and publishes it in
`session.ready.agent`; Dictation resolves the same name from the agent catalog.
Assistant captions carry that name, user captions `You`, and the fallback is
`Assistant`. No backend service name is ever shown as the speaker.

### Wake detector and microphone ownership

- Vosk uses a closed grammar (active phrase plus unknown token). Configured
  and decoded text share normalization; the armed orb renders
  `Say “<phrase>”`. The first configured phrase is authoritative.
- Vosk releases the microphone after a wake hit and reopens after the
  conversation ends, so cooldown is real listening time.
- There is one detector owner, configured only from device-local Orb Settings;
  it hands phrases straight to the lifecycle (no webview wake commands).
- Dictation and PTT use generation-scoped microphone leases; only the matching
  lease may restore wake listening, and response ids stop late output-end
  frames from moving a newer response back to listening.
- Orb conversations claim their transport with a session sequence; pause,
  disarm, cleanup and failed starts end only that sequence, guarded at the
  shared transport seam.
- A wake-stream device failure is terminal and visible. Unexpected realtime
  socket close is terminal unless the server sent its session-ended event.

**Stack bounds:** the transport setup future is heap-boxed at its entry, and
deferred configuration recovery is bounded to two passes. The reducer is
stress-tested through 100,000 exchanges on a 64 KiB stack.

**Lock and power:** while armed, a `keepawake` assertion keeps CPU and
microphone work alive through idle and screen lock without forcing the display
on. Conversation is voice-only while locked, and the current state renders on
unlock. With **Keep wake listening ready
on battery power** off, a 30-second power check disarms on battery and re-arms
on AC.

## Renderer and accessibility

On macOS, `desktop/src/orb/OrbShader.svelte` renders a DPR-aware WebGL2 body,
the same shader at the 32px ear and the 224px center. Fragment alpha is the
outline (no CSS filter or shadow). An FBM domain warp ripples the membrane, a
slow inhale (`uBreath`, about nine seconds while Armed) swells it, and each
phase has its motion profile; graphite states are still. Palette changes blend
over 520 ms.

Linux and Windows use the layered CSS orb and create no GL context
(`hostUsesShaderOrb` is the host check); Mac falls back to it when WebGL2 is
lost.

- **Budgets:** resting ≤ 10 fps; active ≤ 60 fps; DPR capped at 2. Audio
  callbacks write only lock-free RMS envelopes; a 30 fps pump sends them to the
  webview.
- **Reduced motion** zeros the motion profile (palette kept) and also disables
  center-screen travel.

Both the transcript strip and the center Orb show live captions. The center's
actions — **Talk**, **Pause/Resume**, **Today**, **Settings**, **Settle**,
**Home**, **Rest/Wake** — appear after a tap, each with a tooltip, accessible
label and keyboard focus. Final captions use a polite live region (drafts are
not announced). The tray mirrors status and actions. **Rest** stops the
conversation and hides the center Orb; with wake enabled the next phrase
re-arms, and the summon chord is the microphone-free alternative.

## Configuration

```toml
[orb]
enabled = true
wake_enabled = false # off by default; hold Left Option to talk. true lets the wake phrase summon a hidden Orb
voice_mode = "hands_free" # "dictation", "hands_free", or "realtime"
voice_mode_seeded = false # set true once the first backend seed lands; later changes stay local
hotkey = "Alt+Space"
leash_minutes = 120
follow_up_seconds = 8
wake_phrases = ["hey assistant"]
armed_on_battery = true
auto_expand_on_wake = true
```

Settings exposes readiness and wake as independent switches, normalizes
empty/duplicate wake phrases, clamps the leash to **5–240 minutes** and
follow-up to **1–60 seconds**, and rejects a shortcut colliding with Quick
Automate, voice or a screen-capture chord. Saved changes apply without a
restart, preserve the current lifecycle, and do not restart the leash.

## Verification

`make test-desktop-tray` is the provider-free checkpoint: it builds the real
`native-wake` feature after staging Vosk, mounts the Svelte surface against
mocked Tauri events, and pins the lifecycle, voice and geometry rules above.
Placement on notched vs external displays, full-screen float, lock/unlock
continuity, audible turn-taking and GPU/energy use need a real notched Mac plus
a non-notch display; no automated test may claim them.
