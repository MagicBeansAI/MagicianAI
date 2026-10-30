# magesp changelog

## 1.10.0 — 2026-10-25

### Changed
- The face page is the Magican star, the brand mascot: its "hi" pose (star, shine, glasses, arms and legs) is drawn from flash, with live eyes behind the glasses and a live mouth. The eyes widen while the microphone is open, glance about while Magician thinks, and blink; the mouth smiles at rest and moves only while audio plays. The dark theme draws the arms and legs light so they show on the black panel. `tools/star/gen_star_body.py` regenerates the art from `ui/unified-ui/static/brand`.
- A **conversations** page (between For you and voice mode) chooses which Magician thread and chat the terminal talks in: the current choice, **+ new thread**, the recent threads and the chosen thread's chats. A pick is kept in NVS across restarts. Until one is made, turns go to the terminal's own thread, named after it. A new thread is named after the device and the time and filed under Automated. Lists load when the page opens and run on the turn worker, never alongside a voice turn.
- Speech is requested as 16-bit PCM (`wav_encoding: "pcm16"`). macOS speech arrives as float32 at 22.05 kHz, twice the bytes the terminal has to pull over Wi-Fi and TLS while playing, which brought back static and gaps between words.
- Every authenticated request sends `X-Magician-App-Version` (the firmware version), so web Settings → Devices shows which build each terminal runs.

## 1.9.3 — 2026-09-18

### Fixed
- A realtime call started by tapping the face no longer needs a spoken wake prefix.
- During a live call the face shows "just talk" and a long button hold no longer starts a competing push-to-talk turn.
- A finished call or an intentional stop no longer causes the next connection to fail.
- Realtime mode now uses the native realtime provider with server-side turn detection instead of the cascaded hands-free pipeline.
- Control-event parsing no longer depends on a trailing NUL, so the terminal returns from speaking to listening reliably.
- Playback ends after a short audio gap if the end-of-output event never arrives.
- Realtime audio tolerates brief Wi-Fi/TLS stalls instead of dropping the call.
- Realtime can start after a completed or failed push-to-talk turn.
- The local `/config` diagnostic reports realtime state, failure reason and audio counters.

## 1.9.2 — 2026-08-31

### Changed
- **Breaking:** pair once through the ESP bootstrap route and then authenticate with the returned device bearer; principal/workspace configuration and the dedicated device-token header are removed.

## 1.9.1 — 2026-08-14

### Fixed
- Tapping the face in realtime mode starts a call instead of opening the answer panel.
- The idle timer no longer shuts down audio under a running realtime capture.

### Added — the microphone uplink
- Realtime streams microphone audio to the server on its own capture path; mic and speaker take turns on the single codec.

### Known — the call still ends without a reply
- Calls end with no assistant reply because no turn boundary is ever set server-side; not fixed in this release.

## 1.9.0 — 2026-08-14

### Added — realtime is selectable, and a call runs
- Realtime can be selected; tapping the face starts and ends a live session with assistant audio playback. No microphone uplink yet, and the screen says so.

### Step 2, measured — the audio hardware fits beside the socket
- Fixed: audio allocation failures no longer reboot the device.
- Realtime uses smaller DMA buffers so audio fits beside the open socket; the turn path keeps its deep buffers.

### Unchanged
- Dictate and hands-free modes are unaffected.

---

Older entries: `docs/archive/changelogs/magesp.md`
