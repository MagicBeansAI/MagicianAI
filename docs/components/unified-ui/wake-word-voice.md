# Ambient Orb wake word

Wake is an ambient-Orb admission mechanism, not a chat-composer feature. On
desktop, enable it under **Settings → Ambient Orb → Let the wake phrase
summon the Orb**. A valid phrase can summon a fully hidden/disarmed Orb and
starts its selected Dictation, Hands-free, or Live rail. Option+Space
remains the explicit keyboard alternative.

Users name their assistant anything, so a fixed keyword model (one compiled
model per phrase) cannot work. A continuous **on-device STT** (Vosk, fully
local) matches the configured wake word in its **finalized** transcript — the
same approach as the meeting bot's `is_addressed`. The wake word is data.

Admission requires a **finalized phrase-led utterance**: `Hey Presto` and
`Hey Presto, open Today` are valid; `Presto`, `I was talking to Presto`, a
mid-sentence `hey Presto`, and every partial hypothesis are ignored, so ordinary
room speech cannot open capture.

## UI ownership

The chat composer **exposes no Wake button** and never receives an accepted
native wake event.

`VoiceControl` is a **split button on the composer row**: the left half is the
action for the selected mode; the right half (caret, 12px) only opens the mode
menu and is not rendered with `showChevron={false}`.

- **Dictate** (`mode === 'recording'`) — `MicCaptureButton` (tap to record, hold
  to talk).
- **Call / Hands-free** — one `VoiceCallButton` (start/end the selected call);
  no mic in these modes.

The popover carries no action button. It holds the **Hands-free / Call /
Dictate** switch (disabled mid-take so a live recording cannot unmount), a
per-mode hint, the Call realtime profile picker, **Keep dictation recordings**
(Dictate), a scoped **Require "Hey \<assistant\>"** setting (Hands-free and Call,
non-translation; locked during a live call because aliases are negotiated at
session start), and the surface-profile control plus **read-aloud** toggle.
Because the mic is not inside the popover, the popover does not pin itself open
through a take; `onRecordingChange` only collapses it when a take finishes.

Modes:

- **Hands-free** — local cascaded `voice_mode=hands_free`: browser PCM →
  configured VAD/streaming STT → normal Magician agent → configured TTS →
  browser PCM.
- **Call** — vendor Realtime voice; its PTT/open-mic controls are
  provider-local.
- **Dictate** — record → transcribe → send (`MicCaptureButton`).

Desktop Orb wake phrases are device-local in Orb Settings. The browser-only
Warroom ambient surface derives its alias through `effectiveWakePhraseStore`,
registered as an ambient wake target distinct from composer push-to-talk. There,
Dictation returns to a **wake-only armed boundary** after every audible reply
rather than opening general VAD capture; the finalized `Hey <assistant>` phrase
is barge-in during TTS (cancels the reply, admits one bounded turn). The wake
engine is suspended for that capture and resumed only at the actual lifecycle
boundary — never by a fixed timer mid-recording. Tauri does not run this browser
detector: the process-owned Orb detector is the single native microphone owner
and observes the same bounded capture leases.

### In-call address control

Wake starts the selected mode; while a Hands-free or Realtime call is open, the
address control decides which room speech becomes a user turn. It is on by
default via the scoped `require_voice_prefix` media preference. The backend
derives every accepted `Hey <name>` from the primary personal agent's aliases,
canonical name and `wake_spellings`, and returns the frozen set in
`session.ready.addressing`.

**Starting a call must never latch the re-entry guard.** `startVoiceCall` bails
on `if (active || starting)`, so the catch releases `starting` **before** its
generation check; a call that cannot start (unconfigured provider, missing mic
permission) surfaces the reason instead of no-opping.

**The UI reports the gate's real state**, using the backend's exact activation
list (all names are live):

- **Wanting the gate is not having it.** `VoiceAddressing::new` sets
  `required = required && !names.is_empty()`, so with no name available the call
  runs ungated (logged server-side). `voiceCallStore` carries `addressingRequired`
  (the backend's answer, distinct from the preference) plus `activationPhrases`;
  the popover shows "Gated — say …" or an explicit warning that room speech is
  being heard.
- **The gate is fixed at call start** (`voice_control_handler.rs` reads the
  preference once); the checkbox is disabled during a call.

The transcript must begin with a complete phrase token (`Hey Sam, send it` —
not `Samantha` or a mid-sentence `hey Sam`); casing and punctuation are ignored.
The prefix is stripped before captions, Chat, the agent or tools see it. A bare
finalized `Hey Sam` arms one following utterance for the backend-advertised
eight-second window. Direct browser providers run the same matcher locally to
stop racing audio, but Magician is the admission authority; backend-proxied
native clients (including iOS) receive only admitted captions.

Push-to-talk in a connected call: hold the talk button or Spacebar (desktop, PTT
mode, not typing). The PTT ⇄ Live toggle lives in the call UI.

### Hold Left Control + Left Option - one mode-aware voice chord

**Left Control + Left Option** is the universal voice shortcut (a two-key chord
never shadows Option-key typing). While held it follows the mode read **once at
press**: **Call** → Realtime PTT, commit on release; **Dictate** → record,
transcribe, send on release; **Hands-free** → start the continuous cascade (no
synthetic PTT boundaries). See `pushToTalkPress` / `pushToTalkRelease` in
`wakeWord.ts`.

- **Composer mic (pointer PTT, Dictate only).** Tap toggles start/stop; hold is
  push-to-talk. `media/capture/pressGesture.ts` owns only the tap-vs-hold
  decision (pure, injected timers); `MicCaptureButton` keeps the recorder. Taps
  stay on native `click` (keyboard activation unaffected) and the click after a
  hold is suppressed; a release before `getUserMedia` resolves is honored once
  the stream exists. The composer registers `registerRecordingTrigger` for the
  chord's Dictate path; ambient wake uses the Orb-only
  `registerWakeRecordingTrigger`.
- **Browser chord** — `installPushToTalkHotkey` (mounted by `VoiceControl`)
  engages when `ControlLeft` and `AltLeft` are both held and releases when
  either lifts; engagement is deferred ~120 ms and cancelled if a third key
  follows (so `Ctrl+Option+<key>` shortcuts like VoiceOver pass).
- **Desktop** — the native CGEventTap (`desktop/src-tauri/src/voice_gesture.rs`)
  owns the chord and keeps the OS event; the web mirrors the mode via
  `invoke('set_ptt_mode', …)` and the in-page listener is disabled in Tauri. A
  bare single-modifier hold is still selectable in Settings.
  **Hydration gate (`voiceModeHydrated`):** the tray seeds its PTT mode from the
  persisted backend preference, so the web `voiceModeStore` pushes
  `set_ptt_mode` only after `hydrateVoiceModePreference` — a never-hydrating
  webview (e.g. notify-overlay) would otherwise clobber the seed with the
  unhydrated `'realtime'` default.

## How it works

`src/lib/media/voice/wakeWord.ts`:

- Persisted stores (localStorage): `wakeEnabledStore`, `voiceModeStore`,
  `wakePhraseStore`, `wakeModelUrlStore`, `wakeStatusStore`.
- Lazy-imports `vosk-browser`, opens the mic, and feeds a `ScriptProcessor`'s
  buffers to `KaldiRecognizer.acceptWaveform`. Only finalized `result` text goes
  through `finalizedWakePhraseMatches` (`partialresult` is a no-op); a match
  calls `fireWake()` (4 s cooldown).
- While a live call owns capture, the listener **releases its browser mic and
  audio graph**, rebuilding after the terminal-state cooldown (Tauri pauses its
  native listener instead).
- Hands-free and Realtime are wired in-service; Dictate wakes go through
  `registerWakeRecordingTrigger`.

Hands-free browser capture requests echo cancellation, noise suppression and
AGC; the actual track setting is sent to the backend, and when echo
cancellation is not confirmed the session enables half-duplex mic gating while
assistant PCM plays. Starting live voice cancels ordinary TTS and holds audio
focus until a terminal state. Control-socket open/close/stop races tear down
partial mic, playback and audio-context resources; opening times out after 15 s.
Recoverable server warnings attach to the connected call without changing its
state; fatal errors transition to error and release everything.

## Native desktop wake (Tauri)

In the desktop app the detector runs **natively in Rust** (`vosk` crate,
`desktop/src-tauri/src/voice_wake.rs`) because the in-page audio graph is
throttled when the window is backgrounded. A `cpal` stream feeds Vosk on a
background thread; an accepted phrase goes directly to the process-owned Orb. If
the Orb is off/ended/disarming, the handler re-arms it persistently and then
consumes the same utterance as the ordinary wake transition. No webview event,
composer fallback, or page-controlled enable/phrase command exists.

The listener is behind the **`native-wake` cargo feature** (off by default, so
plain builds/tests need no `libvosk`); `make build-desktop-tray-debug` /
`release-desktop` pass `--features native-wake`. Without it the controller and
commands exist but the listener is a no-op.

`orb.wake_enabled` may stay true while `orb.enabled` is false: disarm hides the
surface and stops its conversation, then reopens only the lightweight detector.
Active calls and bounded captures suspend it; normal release resumes it. Battery
policy and mic/session failures fail closed. Setup (`libvosk` + an unpacked
model) is in the desktop README.

## Preloading & caching

The browser Vosk model serves only the browser Warroom ambient surface:

- **No root/composer preload** — ordinary tabs never download the model.
- **Never in Tauri** — `startWakeWord()` returns without loading a model or
  opening a mic; the native Orb listener is the sole desktop owner.
- **Kept warm across toggles** — turning wake off tears down only the mic and
  audio graph; `disposeWakeModel()` frees the model if needed.
- **Persistence is vosk's own extracted-model store, keyed by a stable URL** —
  vosk-browser persists the extracted model in IDBFS (IndexedDB `/vosk`) keyed by
  a directory derived from the model URL and skips download and untar when it
  exists (`extracted.ok`). `loadModel()` therefore passes the **direct model URL,
  never a blob URL**. A one-time versioned reset
  (`magician.voice.wakeFsVersion`) drops the legacy store and the obsolete
  `vosk-wake-model-v1` Cache Storage bucket.
- **Bounded** — `createModel()` hangs on a corrupt source, so init has a 120 s
  timeout surfacing an `error` status; an env probe logs `wasm=…/blobWorker=…`
  and success logs `[wake] model ready in Xs`.

## Setup

The Vosk model is **not** committed (tens of MB). Once per environment:

```bash
cd ui/unified-ui && npm install        # adds vosk-browser
```

Then place a small model tarball at `ui/unified-ui/static/vosk/model.tar.gz`
(see `ui/unified-ui/static/vosk/README.md`). Override the path via
`wakeModelUrlStore` to load from a CDN.

Design: `docs/archive/plans/2026-06-08-wake-word-in-app-voice.md`.
