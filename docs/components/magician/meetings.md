# Meetings

An agent that joins a meeting, hears it, and can speak into it — rather than a
recorder that hands you a transcript afterwards.

## Setup

`make install` runs this; `make setup-meet-bot` runs it on its own.

**macOS, to speak.** BlackHole 16ch (`brew install --cask blackhole-16ch`) is
a virtual microphone. The joiner switches the system default input to
`BlackHole 16ch` for the meeting and restores it on leave; point Chrome's
microphone there too if Chrome does not follow the system default. The cask
needs an administrator password and a reboot before Core Audio lists the
device. `switchaudio-osx` (which provides `SwitchAudioSource`) is installed
alongside.

**macOS, to listen.** ScreenCaptureKit through
`magician-macos-meet-audio.bin`, built by `make build-macos-speech-helper`
(also by `make build-all-debug` / `make build-all-release`, and by
`make setup-meet-bot` when missing). Grant Screen Recording to the app that
runs the bot. BlackHole is not the listen path; BlackHole 2ch is not used.

**Linux.** No kernel audio driver: join, listen and speak use PulseAudio, or
PipeWire through `pipewire-pulse`. `make setup-meet-bot` installs `pactl`,
`parec`, `pacat` and Xvfb, and installs a PipeWire Pulse server only when no
PulseAudio or PipeWire-Pulse package is installed (an installed `pulseaudio`
package stays even if this shell cannot see the desktop server). On join the
bot creates two null-sinks: `magician_meet_mic` (its voice, taken by the browser as the
microphone) and `magician_meet_capture` (the browser's output, when Pulse
attributes it). The previous default sink is restored after creating them.
Leave moves the browser's streams off the capture sink and restores the
desktop microphone and sink only when no other attendee is in a call; a saved
microphone that is `magician_meet_mic.monitor` is replaced by a hardware
source. If the browser stream is not visible at capture start, the bot records
the desktop sink's monitor and runs half-duplex. The passive listener skips
`magician_meet_mic.monitor`. A desktop session has `DISPLAY`; a headless host
gets Xvfb on `:99`. The container image does not start Pulse; the bot must run
on the same machine as the Pulse server.

## Two audio seams

[`media_seam/meeting_audio.rs`](../../../magician/src/magician_v2/media_seam/meeting_audio.rs)
defines the meeting as two directions, implemented with ScreenCaptureKit and
CoreAudio on macOS and `parec` / `pacat` on Linux:

- **`AudioSource` — the meeting into the bot's ears.** Pushes PCM16 chunks to
  the `StreamingSttProvider` and the wake-word detector, which keeps the bot
  quiet until addressed.
- **`AudioSink` — the bot's voice into the meeting mic.** Injects synthesized
  PCM16 so participants hear the reply.

`MeetingSession` is written against those traits, so it tests against no-op
implementations. `magician-macos-meet-audio.bin` is the production replacement
for the `scripts/meet-bot/transcribe_loop.py` ffmpeg/sox spike.

## Joining

The [`cloak-browser`](../../../skillshub/cloak-browser/SKILL.md) skill swaps
the `browser` tool's engine for a Chromium build with fingerprint resistance
compiled in: canvas/WebGL noise, GPU spoofing, `navigator.webdriver` removal,
TLS ja3n/ja4 matching, CDP-detection removal. It is an engine swap, not a new
capability; the agent calls `browser` as before.

Linux joins with the same flow, headed under `DISPLAY` or Xvfb, with audio
through Pulse. A container hears the meeting only when Pulse runs in it.

## WhatsApp Web voice calls

`POST /api/magician/v2/meetings/join` treats a `web.whatsapp.com` URL as a
voice call on the same attendee. The microphone stays BlackHole 16ch (Linux:
`magician_meet_mic`); the speakers stay the Mac speakers or headphones so
ScreenCaptureKit hears the other person. Do not point the speaker at BlackHole.

| URL | What the bot does |
| --- | --- |
| `https://web.whatsapp.com/` | Waits, then answers the next incoming voice call. |
| `https://web.whatsapp.com/send?phone=<digits>` | Opens that chat and places a voice call. `phone` must be 8–15 digits. |

A call is recognized only when the page shows both Accept (or Answer) and
Decline (or Reject); Accept alone is not a call. While that pair shows,
`GET /meetings/{id}` reports `IncomingCall`. The bot clicks Accept, and the
status becomes `Listening` once End call is visible. It never clicks the Calls
tab or video.

The answer wait defaults to 10 minutes (`MAGICIAN_WHATSAPP_CALL_WAIT_SECS`,
clamped to 30–1800); a placed call has 90 seconds to reach End call. The first
run of the meet-bot Chrome profile (not everyday Chrome, not the WhatsApp text
bot) may show a QR code to scan once. Without a profile in the join, the voice
engine is `MEET_BOT_REALTIME_PROFILE`. The Observe Join form and Settings send
`realtime_profile` from the composer Call menu's catalog, kept in the browser,
default `voice_realtime_openai_backend`. The call is a realtime session; the
model hears the room and takes turns. Dictation and hands-free are not used.

### Already-open Chrome

The Magican extension and the Observe **Now** pane use a second path that
launches no browser and does not change the Mac microphone.

`GET` and `POST /api/magician/v2/meetings/whatsapp-ring` share one ring. The
extension posts `incoming` when Accept and Decline both show and `ongoing`
when End call is up. The caller name comes from the call screen, never the
unread page title.

When Observe's WhatsApp card has **Show call buttons in the browser** on (the
default), every tab shows a banner with the caller and four buttons. Turning
it off only hides them.

| Button | Effect |
| --- | --- |
| **Join as Magican** | Clicks Accept in the WhatsApp tab and starts the meeting voice loop on Chrome's ScreenCaptureKit bundle, like a Meet join. Ringing calls only. |
| **Listen** | Clicks Accept if still ringing, then transcribes with the passive listener (`capture_mic` off, no voice, no system-mic change). Offered instead of Join once a call is up. |
| **Ignore** | Hides the banner on every tab without clicking Decline; Observe still shows the call. |
| **Cancel** | Clicks Decline while ringing, or End call / Hang up / Leave call once up, and tells Magician `cancel`, which also stops Magician's side if it joined or is starting. |

Observe's card offers Join as Magican and Listen while ringing, Listen while a
call is up, and nothing when idle; its Join clicks Accept in the open WhatsApp
tab and starts the voice. Leave and Stop listening click End call,
Hang up or Leave call and post `ended`; End call disappearing after it was
seen also posts `ended`. A join shows as an attendee in the active list, a
listen as passive listening; a `listen-*` session stops on the passive rail.

Join's mouth is still BlackHole 16ch. WhatsApp and Google Meet both stream
capture to the realtime model as it arrives, with server turn detection
(`turn_boundary: server_vad`); dictation and hands-free are not offered.
Capture is not sent back while Magician speaks, so its voice never becomes the
next question.

**Who answers.** Google Meet seats Envoy (`meeting_bot`). A WhatsApp call
seats the personal assistant (`web_desktop`) only when the other party is a
phone number matching the owner number (`WHATSAPP_SELF_JID`, `WHATSAPP_PHONE`
or the linked wu account); a contact name seats Envoy. A public call keeps that
agent's own tools; `delegate_to_chat` runs as the same agent, and search cannot
use the owner's signed-in Chrome. An untrusted call gets no learned
procedures, does not advertise `delegate_to_agent` or `handover_to_agent`, and
does not take the scope voice slot that speaks cockpit diff approvals. The
signed-in bearer on the ring request is the voice session token and is never
written into the process environment.

**Engine.** Any selectable realtime profile from the composer Call menu's
catalog (`GET /media/providers`), shared by Observe and Settings. The stored
default is `voice_realtime_openai_backend` (GPT Realtime 2.1) and applies to
the next call. An id outside the catalog stays on the default; a change while
a call is starting, live, listening or leaving is ignored. Listen never speaks.
`POST /meetings/join` is unchanged.

## Around the meeting

- **Before** — the [`meeting-prep-brief-format`](../../../skillshub/meeting-prep-brief-format/SKILL.md)
  skill shapes the prep brief.
- **After** — `feed_api` derives follow-ups from memory and knowledge
  (`today_meeting_followup_items_from_memory`,
  `today_meeting_followup_items_from_knowledge`) into Today's **Follow-ups**
  lane, so meeting obligations become tracked items. See
  [Today Attention Lanes](../unified-ui/today-attention-lanes.md).

## Surfaces

| Surface | What is there |
| --- | --- |
| Web | `/meetings/[thread]` |
| Android | `MeetingModels.kt`, `MeetingsRepository.kt`, `MeetingsViewModel.kt` in the magdroid bridge |
| macOS | the native Core-Audio meet-audio helper |
