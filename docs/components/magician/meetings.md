# Meetings

An agent that joins a meeting, hears it, and can speak into it — rather than a
recorder that hands you a transcript afterwards.

## Setup

`make install` runs this. `make setup-meet-bot` runs it on its own.

**macOS, to speak.** BlackHole 16ch (`brew install --cask blackhole-16ch`).
It is a virtual microphone. The joiner switches the system default input to
`BlackHole 16ch` for the meeting and restores the previous input on leave.
Point Chrome's microphone at `BlackHole 16ch` as well when Chrome does not
follow the system default. The cask needs an administrator password and a
reboot before Core Audio lists the device. `switchaudio-osx` is installed
alongside it; that provides `SwitchAudioSource`.

**macOS, to listen.** ScreenCaptureKit through
`magician-macos-meet-audio.bin`, built by `make build-macos-speech-helper`
(also part of `make build-all-debug` / `make build-all-release`, and of
`make setup-meet-bot` when the binary is missing). Grant Screen Recording to
the app that runs the bot. BlackHole is not the listen path; BlackHole 2ch
is not used.

**Linux.** There is no kernel audio driver. Join, listen, and speak use
PulseAudio, or PipeWire through `pipewire-pulse`. `make setup-meet-bot`
installs `pactl`, `parec`, `pacat`, and Xvfb. It installs a PipeWire Pulse
server only when no PulseAudio or PipeWire-Pulse package is already
installed; an installed `pulseaudio` package is left in place even if this
shell cannot see the desktop server. On join the bot creates two null-sinks:
`magician_meet_mic` (its voice, which the browser takes as the microphone)
and `magician_meet_capture` (the browser's output, when Pulse will attribute
it). Creating them saves the previous default sink and puts it back, so a
new null-sink does not stay the desktop output. Leave moves that browser's
streams off the capture sink, and restores the desktop microphone and sink
only when no other attendee is still in a call. A saved microphone that is
already `magician_meet_mic.monitor` is not written back; a hardware source
is chosen instead. If the browser stream is not visible when capture starts,
the bot records the desktop sink's monitor and the session runs half-duplex.
The passive listener's microphone skips `magician_meet_mic.monitor`. A desktop session already has `DISPLAY`. A
headless host gets Xvfb on `:99`. The container image does not start Pulse
itself; the bot process has to be on the same machine as the Pulse server.

## Two audio seams

[`media_seam/meeting_audio.rs`](../../../magician/src/magician_v2/media_seam/meeting_audio.rs)
defines the meeting as two directions. macOS implements them with
ScreenCaptureKit and CoreAudio. Linux implements them with `parec` and `pacat`:

- **`AudioSource` — the meeting into the bot's ears.** Captures meeting output
  and pushes PCM16 chunks toward the `StreamingSttProvider` and the wake-word
  detector. The wake-word path is what lets a meeting bot stay quiet until
  addressed rather than narrating over everyone.
- **`AudioSink` — the bot's voice into the meeting mic.** Injects synthesized
  PCM16 so participants hear the response in the room.

`MeetingSession` is written against those traits, so it stays testable against
no-op implementations independently of the native bridge. The bridge itself
(`magician-macos-meet-audio.bin`) is the production replacement for the
`scripts/meet-bot/transcribe_loop.py` ffmpeg/sox spike path.

## Joining

Joining a web meeting means driving a browser that does not announce itself as
automation. The [`cloak-browser`](../../../skillshub/cloak-browser/SKILL.md)
skill swaps the `browser` tool's engine for a Chromium build with fingerprint
resistance compiled in — canvas/WebGL noise, GPU spoofing, `navigator.webdriver`
removal, TLS ja3n/ja4 matching, CDP-detection removal. It is an engine swap
under the existing tool, not a new LLM-facing capability: the agent keeps calling
`browser` the same way.

Linux joins with the same browser flow as macOS, headed under `DISPLAY` or
Xvfb, and routes audio through Pulse instead of BlackHole. A container only
hears the meeting when Pulse is running in that container.

## Around the meeting

- **Before** — the [`meeting-prep-brief-format`](../../../skillshub/meeting-prep-brief-format/SKILL.md)
  skill shapes the prep brief.
- **After** — `feed_api` derives meeting follow-ups from memory and knowledge
  (`today_meeting_followup_items_from_memory`,
  `today_meeting_followup_items_from_knowledge`) and lands them in Today's
  **Follow-ups** lane, so obligations made in a meeting become tracked items
  rather than notes. See [Today Attention Lanes](../unified-ui/today-attention-lanes.md).

## Surfaces

| Surface | What is there |
| --- | --- |
| Web | `/meetings/[thread]` |
| Android | `MeetingModels.kt`, `MeetingsRepository.kt`, `MeetingsViewModel.kt` in the magdroid bridge |
| macOS | the native Core-Audio meet-audio helper |
