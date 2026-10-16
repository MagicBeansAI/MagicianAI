# magesp changelog

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

## 1.8.0 — 2026-08-14

### Added — realtime step 1: the socket, and what it costs
- Experimental realtime control socket with heap reporting at each stage; no audio yet and only runs when asked. A live session fits in memory and frees fully on close.
- Larger websocket header buffer and task stack, needed for the TLS handshake through an access proxy.

### Unchanged
- Dictate and hands-free modes are unaffected; realtime still cannot be selected.

---

Older entries: `docs/archive/changelogs/magesp.md`
