# Mac Notch Orb

Magician Desktop owns an ambient voice surface that rests as a small tab on the
right screen edge, under the menu bar. It can grow into an expanded panel at top
center and make a center-screen arrival before settling back to that side home.
It is never painted as an application window: the native panel is borderless and
transparent, and only a soft radial wash (for readability over the wallpaper;
it fades out), the Orb, its halo, transient state/caption clouds and explicitly
summoned radial actions produce pixels.

## Product contract

The orb is a truthful projection of the process-owned voice lifecycle.
`Armed`/`Disarming` are internal names; product copy says **Ready**,
**Resting**, **Rest** and **Wake**. Resting does not claim all microphone work
is off: with wake phrases enabled, the on-device detector may remain ready.

| Phase | Meaning | Palette |
|---|---|---|
| Armed | Wake detection is ready; conversation capture is not active | Armed ember |
| Heard / connecting | A wake was accepted and the selected voice session is opening | Violet surge |
| Listening | The native conversation input stream is active | Calm aurora |
| Thinking | Speech ended or muted assistant output is being prepared | Amber |
| Speaking | Audible assistant PCM is being produced locally | Teal |
| Voice busy | Another native voice capture owns the microphone; wake is suspended | Graphite |
| Paused / disarming / ended | No ambient capture is claimed | Graphite |

Colors numerically mirror `magios/Shared/AmbientOrbAppearance.swift`. Motion is
decorative and reactive but cannot select a phase: `orb_state.rs` is the
authority, and the `/orb` webview only renders its snapshots and events.

### How a conversation starts

With `orb.enabled`, startup enters **Armed** and shows the side pill. Wake
listening is off unless `orb.wake_enabled`. The default is **holding Left
Option** (whose double-tap opens Quick Automate): the microphone is open only
while held, and release commits the turn. In every mode the microphone closes
between holds; a Live socket stays connected so the next hold skips setup.

When the visible Orb is off but `orb.wake_enabled` is true, only the on-device
Vosk spotter runs, and a valid phrase cold-starts the Orb. It reads the
microphone to recognize the phrase but retains no audio, submits no speech and
opens no media session or conversation rail.

A conversation starts only from a finalized utterance beginning with the wake
phrase, an explicit **Talk** from the radial actions, or a Left Option hold.
Partial recognition, startup microphone settling, mid-sentence mentions,
mounting `/orb`, and expanding/collapsing cannot start capture.

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

The webview subscribes before rehydrating lifecycle and presentation; both
snapshots carry monotonic revisions so a late getter cannot overwrite a newer
event. The compact status pill and terminal farewell are mutually exclusive:
during the bounded exit window the farewell is the single visual and
`aria-live` owner. User-directed rest shows compact **Resting** copy.

### Window and placement

`orb_window.rs` projects state into a `tauri-nspanel` non-activating `NSPanel`
at status-bar level: all Spaces, beside full-screen apps, outside the app window
cycle.

- **Compact:** a tab on the right edge just under the menu bar (real notch depth
  is measured so menu titles and status icons stay clickable). At rest only the
  Orb shows; hover or an active turn slides the status line out.
- **Startup:** each launch opens the large Orb at center with the currently
  configured shortcuts (overlay-key hold and double-tap, Orb chord, wake phrase
  if on), then settles to the tab; a resting Orb hides after that.
- **Expanded** grows at top center; **Spotlight** is the center-screen arrival.
  High-salience wakes may animate into a 620×500 spotlight for 1.25 s, then
  travel 520 ms (ease-out) back to the pill. A tap opens the 480×300 detail
  scene and reveals radial actions (a second tap toggles them). Double-click
  grows the Orb into the persistent spotlight. **Settle**, Escape and the global
  shortcut return to the pill without disarming.
- **Drag:** compact and expanded surfaces (including the Orb itself) can be
  dragged on notched and notchless displays. Detaching turns the wash into the
  floating treatment without changing body size or content alignment (the
  native `docked` projection owns only the wash; presentation owns content
  geometry). Dropping in the side-home magnetic zone, or **Home** on the action
  wheel, snaps back to the tab. Detached normalized positions persist in
  device-local config across collapse, expansion and restart; Spotlight/Settling
  motion never overwrites a parked position.
- **Focus:** after `tauri-nspanel` converts the window, focusability is owned by
  the panel's `canBecomeKeyWindow`, non-activating style and cursor gating. Do
  not use Tauri's focusability setter here — Tao implements it via a private ivar
  on its original window class. Direct AppKit use is limited to
  `NSScreen.safeAreaInsets` and the two auxiliary top-area rects (notch metrics
  Tauri does not expose). The built-in display is preferred; following the
  active external display is out of scope.

## Voice and lock behavior

The orb reuses Desktop's native PCM capture/playback and voice transports in
`voice_note.rs`. **Settings → Ambient Orb → Conversation Mode** chooses
**Dictation**, the backend's `hands_free` FluidAudio pipeline, or the configured
`realtime` provider, independently of other app voice surfaces. Legacy
`recording`/`dictate` normalize to `dictation`; invalid values fail closed to
`hands_free`.

**Seeding:** the first authoritative backend media preference seeds this
device-local field, then `voice_mode_seeded` stops later shared changes from
overwriting it. The startup GET may not seed (it can race the backend's
saved-preference load and return compiled defaults): Desktop waits for the
realtime preference stream, re-fetches behind that barrier, and only that
snapshot, an explicit save or a later preference event may seed. Choosing a
mode in Settings marks the seed complete.

**Admission:** a conversation snapshots the mode once; Hands-free/Live carry
that engine through async media-session registration, so a racing Settings edit
applies only to the next conversation.

### Dictation

A complete iterative turn loop: record one utterance, detect its boundary,
submit through the canonical voice-note STT/agent path, render final captions,
synthesize through the backend TTS profile, and open a fresh capture. Speech ends
after **1.1 s** trailing silence or a **45 s** utterance cap; a capture with no
speech ends at `follow_up_seconds`; network processing has a **3-minute**
ceiling. Each turn completes before the next (an explicit loop, not recursion),
so a long armed window has constant stack depth and one microphone owner. All
synthesized segments in an answer share one native prebuffer and drain boundary.

Recorded-turn 422s are typed: `unsupported_language`, `empty_transcript` and
`no_speech` show (and speak, if output is on) the backend guidance and open the
next bounded capture with the same chat session. Unknown codes, transport and
infrastructure errors end the conversation — no unbounded retry loop.

### Hands-free and Live

Both use the native conversation WebSocket. Hands-free prepares one upcoming TTS
segment while current PCM streams (bounded producer lead).

- **Half-duplex Hands-free:** raw macOS CPAL capture has no echo cancellation,
  so the desktop withholds microphone frames while native playback sounds and
  for a **400 ms** tail after it drains; the backend enforces the same boundary
  before local and cascaded STT. Live realtime stays full-duplex with barge-in;
  muted Hands-free output does not gate the microphone.
- **Quiet deadline:** `follow_up_seconds` starts when the session is ready, is
  cancelled by user speech or assistant output, and re-arms only after a
  response or a textless/rejected turn completes. Microphone frames and room
  noise never refresh it. On expiry capture and socket close, the wake detector
  resumes, and the cooldown settles to Armed.
- **Turn boundary:** a Left-Option Live session omits the `turn_boundary`
  override, so the backend realtime profile's manual push-to-talk boundary
  commits on release; the next hold sends `ptt.engage` on the same session.
  Wake-word and Talk Now Live sessions snapshot `server_vad`, since they have no
  release gesture.
- **Backend-proxied Live readiness:** microphone frames wait in a bounded
  pre-ready queue until the provider websocket is consuming (OpenAI handshake
  8 s deadline; control-actor readiness fence 10 s). The first wake sends only
  input engagement and cannot cancel a nonexistent response. Barge-in is
  response-aware: `response.created` opens one interrupt permit, duplicates
  coalesce, `response.done` closes it; a stale "no active response" ack is a
  benign race. Configuration and resume replay use the actor-ordered
  non-blocking dispatcher so readiness cannot deadlock behind a full channel.
- **Failover:** a fatal Mac Orb Live failure triggers exactly one
  sequence-checked failover to Hands-free via a fresh scheduler task (never a
  recursive await). A Hands-free failure is terminal.

### Playback

TTS envelopes keep provider/model identity through playback diagnostics.
Playback accepts provider PCM16 WAV and the Float32 WAV from Apple's
`AVSpeechSynthesizer.write` (downmixed/converted onto the mono PCM16 rail). A
trailing incomplete sample/channel frame is aligned to the last complete frame,
and a fully collected streamed WAV with the `0xFFFFFFFF` unknown-length sentinel
is canonicalized to its bounded body (bounded, iterative scan). Oversized or
truncated declarations, unsupported formats and wider corruption stay terminal
errors naming the provider/model.

Hands-free and Dictation share a jitter buffer: output starts after **180 ms**
pre-roll, backpressure follows queued audio duration (not wall-clock sleeps),
short final tails are released explicitly, and playback drains before teardown.
Hands-free keeps a bounded **300 ms** lead. Underrun and dropped-sample telemetry
is logged per segment.

### Captions and speaker identity

A VAD boundary with no finalized text is normal: the backend emits
`transcript.user.cleared`, desktop removes only the unfinished user caption, and
the Orb returns to Listening silently. `transcript.user.ignored` is
reason-aware: self-echo is silent, a prefix-only turn prompts to continue, a
missing prefix shows the backend-advertised activation phrase; unknown reasons
stay visibly fail-safe, and older backends' cleanup reasons stay silent.

The backend resolves the primary agent once at admission and publishes it in
`session.ready.agent`; Dictation resolves the same scoped name from the agent
catalog once per conversation. Assistant captions carry that name, user captions
`You`; without an identity the fallback is `Assistant`. No path exposes a
backend service name as the speaker.

### Wake detector and microphone ownership

- Vosk uses a closed grammar (active phrase + unknown token), not unrestricted
  decoding. Configured and decoded text share normalization; the armed orb
  renders `Say “<phrase>”`. The first configured phrase is authoritative.
- Vosk releases the microphone after a wake hit; the conversation capture takes
  ownership, and the detector reopens after the conversation ends, so cooldown is
  real listening time.
- The process has exactly one detector owner, configured only from device-local
  Orb Settings; it hands accepted phrases directly to the lifecycle (no webview
  phrase/toggle/wake commands).
- Dictation and PTT capture use generation-scoped microphone leases; only the
  matching lease may restore wake listening, and response ids stop delayed
  output-end frames from moving a newer response back to listening. The lease is
  projected through every arm/resume/timer/settings path via a lock-free
  orb-enablement mirror.
- Orb conversations claim their transport with a concrete session sequence;
  pause, disarm, stale cleanup and failed starts end only that sequence. The
  generation guard sits at the shared transport seam so compatibility callers
  cannot tear down a newer Orb conversation.
- A wake-stream device failure is terminal and visible. Unexpected realtime
  socket EOF/close is terminal unless the server sent its session-ended event.

**Stack bounds:** the handoff heap-boxes the selected transport's setup future at
its entry boundary so the lifecycle task does not inherit its state machine;
deferred configuration recovery is iterative and bounded to two passes. The pure
reducer is stress-tested through 100,000 exchanges on a 64 KiB stack.

**Lock and power:** while armed, a `keepawake` assertion keeps CPU/microphone
work alive through idle and screen lock without forcing the display on. macOS
loginwindow owns the lock screen, so conversation is voice-only while locked and
the current state renders on unlock. With **Keep wake listening ready on battery
power** off, a 30-second power check disarms on battery and re-arms on AC.

## Renderer and accessibility

`desktop/src/orb/OrbShader.svelte` renders a DPR-aware WebGL2 body (sharp at the
36-pt compact, expanded and 224-pt Spotlight sizes with live uniforms):
four-octave FBM displacement and angular lobes mutate the silhouette; slower
fields move the aurora, highlight, rim and halo; plus an iridescent angular
gradient, inner light and bloom. Phase motion profiles: low-power respiration
(Armed), fast bloom (Heard), input-envelope membrane pulls (Listening), inward
folding (Thinking), output-envelope growth/ripples (Speaking); Graphite terminal
states are still. Palette changes blend over 520 ms.

- **Budgets:** resting/ended ≤ 10 fps; active ≤ 60 fps even on ProMotion; DPR
  capped at 2; envelope decay normalized to elapsed time. Audio callbacks write
  only lock-free RMS envelopes; a 30 fps async pump serializes to the webview.
- **Fallback:** on WebGL2 loss or setup failure a layered CSS body keeps phase
  color, organic border-radius mutation, internal flow and breathing.
- **Reduced motion** zeros the motion profile (palette kept) and is mirrored to
  the native window controller, disabling center-screen travel too.

The compact Orb projects every phase through palette, status halo, meter, orbit,
breathing and audio-reactive rings; the expanded scene adds live captions
without a rectangular card. Actions — **Talk**, **Pause/Resume**, **Today**,
**Settings**, **Settle**, **Rest/Wake** — appear only after a tap, each with a
tooltip, accessible label and keyboard focus. Final captions use a polite
live region (streaming drafts are not announced). The tray mirrors status and
actions. **Rest** stops the conversation and hides the panel; if wake remains
enabled the detector's next phrase re-arms, and the summon chord is the
microphone-free alternative.

## Configuration

```toml
[orb]
enabled = true
wake_enabled = false # off by default; hold Left Option to talk. true lets the wake phrase summon a hidden Orb
voice_mode = "hands_free" # "dictation", "hands_free", or "realtime"
voice_mode_seeded = true  # first backend seed completed; later changes stay local
hotkey = "Alt+Space"
leash_minutes = 120
follow_up_seconds = 8
wake_phrases = ["hey assistant"]
armed_on_battery = true
auto_expand_on_wake = true
```

Settings exposes readiness and native wake as independent switches, normalizes
empty/duplicate wake phrases, clamps the leash to **5–240 minutes** and
follow-up to **1–60 seconds**, and rejects a shortcut colliding with Quick
Automate, voice or any screen-capture chord (the field is a recorder). Saved
changes apply without a container restart, preserve any active, paused or
disarming lifecycle, and do not restart the leash.

## Verification

`make test-desktop-tray` is the provider-free checkpoint: it builds the real
`native-wake` feature after staging Vosk, mounts the actual Svelte surface
against mocked Tauri events, and pins the Rust lifecycle, voice and geometry
rules above. Placement on notched vs external displays, full-screen float,
lock/unlock continuity, audible turn-taking and GPU/energy use need a real
notched Mac plus a non-notch display; no automated test may claim them.
