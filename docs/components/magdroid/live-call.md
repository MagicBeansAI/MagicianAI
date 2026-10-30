# Live call panel (Android)

The card that replaces the chat composer while a realtime or Hands-free call
runs. It mirrors iOS `magios/Magios/VoiceCallPanel.swift`; the mic-mode
control and its vocabulary are shared with iOS and web.

Code: `magdroid/android/app/src/main/kotlin/ai/magicbeans/magdroid/ui/LiveCallPanel.kt`
(`LiveVoicePanel`, `LiveCallComposerStrip`), used by `ChatScreen.kt` and
`vibedev/VibeDevChatComposer.kt`. State comes from `RealtimeVoiceSession`
(`bridge/.../voice/RealtimeVoiceSession.kt`), with the caption reducer and mic
meter in `bridge/.../voice/RealtimeVoiceLiveCall.kt`.

## Layout

- **Card**: the composer surface colour, a neutral 1 dp card-border stroke,
  `Lively.composerRadius`, 12 dp padding. It slides up from the bottom and
  fades in, and slides down when the call ends or collapses to typing.
- **Header**: a 44 dp aurora orb; the title `<engine> voice` (14 sp bold) with
  an `m:ss` timer (11 sp, theme mono font); the status line (12 sp) below; a
  compact keyboard button that collapses the panel into the composer; and a
  34 dp red rounded-square **X** that ends the call. Voice settings are not
  reachable from the call header; they stay in the composer's Live chevron.
- **Orb**: a port of iOS `AuroraBlobShape`, a circle whose rim follows a
  2θ/3θ/5θ sine mix. It drifts at 0.6 rad/s, the lobes deepen and the orb
  swells with the mic level, and a halo breathes under it. Colours are the
  iOS `AuroraPalette` presets: violet surge while connecting, graphite once the
  call is over, armed ember while the mic is closed (muted, or hold-to-talk not
  held), calm aurora while it is live. With animations off (animator duration
  scale 0), the drift and the breathing stop; the level still moves the orb.
- **Captions**: a fixed 72 dp viewport that follows the newest line. Each line
  starts with a bold 10 sp **You** or the primary agent's name (the product
  name when there is none). Partial lines show at 72 % opacity. Before anyone
  speaks it shows a hint ("Start talking. Captions will appear here." or "Hold
  the button and talk.").
- **Errors**: a danger-tinted row under the header.

## Mic mode

The terms are exactly **Open mic** and **Hold to talk**.

- A two-segment control `[Mic · Open mic | Hand · Hold to talk]`. The active
  segment is filled with `AccentFill` and uses on-accent text; the inactive
  one is transparent with secondary text. Tap the inactive segment to switch
  modes in the same call (`setPushToTalk`). The segments have `Role.Tab` and
  selected semantics. **Hold to talk** is disabled for a realtime translation
  profile (`holdToTalkAvailable`).
- The main button below takes the shape of the mode. In **Open mic** it is a
  round 56 dp Mic/MicOff button with a ring that moves with the level, and
  "Listening · tap to mute" or "Muted · tap to unmute" under it. In **Hold to
  talk** it is a wide capsule that turns accent-filled and reads "Release to
  send" while it is pressed. Press opens the gate; release commits the turn.
- The status line names the mode: "Open mic · Listening", "Open mic · Muted",
  "Hold to talk · Ready", "Hold to talk · Sending" while held. It reads
  "Working…" while the assistant is still on a request, and "Preparing…" while
  the call connects.
- The composer strip, shown while the panel is collapsed for typing, reads
  "Live · Open mic", "Live · Muted" or "Live · Hold to talk", with a matching
  Mic, MicOff or hand icon. It never uses a speaker glyph for the microphone.

## State (`RealtimeVoiceState`)

| Field | Meaning |
|---|---|
| `micLevel` | Smoothed 0..1 level from RMS over the 20 ms PCM16 capture chunks (−50..0 dBFS). It rises fast, falls slowly, and is published at most every 60 ms (about 16 Hz). It is 0 while muted or while hold-to-talk is not held. |
| `connectedAtMillis` | `SystemClock.elapsedRealtime()` at the first `session.ready`. It survives reconnects, so the timer shows the length of the whole call. |
| `captions` | The last 6 caption lines (`RealtimeVoiceCaption`). Partials are replaced in place, and `transcript.user.cleared`/`.ignored` drop the speaker's unfinished line. |
| `holdToTalkAvailable` | False for a realtime translation profile. `setPushToTalk(true)` is refused when it is false. |

The level updates about 16 times a second. Screens collect
`state.withoutMicLevel()` and pass `liveMicLevel()` as a lambda that only the
orb and ring read inside their draw code, so a level change redraws them
without recomposing the card or the chat. The capture loop writes the level
with `MutableStateFlow.update`, which is atomic, so it never overwrites a
concurrent caption or phase write.
