# magesp changelog

## 1.9.3 — 2026-09-18

### Fixed
- **An explicit realtime call no longer needs a repeated wake prefix.** Tapping
  the face is the addressing gesture, so ESP `session.start` now sends
  `require_voice_prefix: false`. This prevents valid speech from being silently
  rejected when transcription clips or mistranscribes the first word.
- **Realtime owns the microphone UI.** The face says `just talk` during a live
  call instead of `hold to talk`, and a long BOOT hold is consumed while the
  realtime stream owns the codec. This prevents a competing push-to-talk turn
  from reporting a false connection failure during a healthy call.
- **A completed call cannot poison the next connection attempt.** Websocket
  connected/closed event bits are reset for every call, and an intentional
  user stop no longer records the normal close callback as a connection error.
- **Realtime now selects the native provider it names.** `MODE_REALTIME` sent
  `voice_mode: hands_free`, which made Magician choose the separate cascaded
  FluidAudio pipeline and ignore `voice_realtime_openai_backend`. It now sends
  `voice_mode: realtime` with `turn_boundary: server_vad`, reusing the existing
  backend-proxied realtime transport and giving streamed microphone audio an
  automatic turn boundary. The ESP32 still yields its half-duplex codec to
  assistant PCM because the C6 has no supported AEC.
- Websocket control-event matching is bounded by the received frame length;
  `session.ready` and Magician's public `audio.output.ended` event no longer
  rely on an undocumented trailing NUL from the ESP websocket client. The
  terminal now returns from speaking to listening when that normalized event
  arrives.
- If a provider adapter accounts for a completed response without forwarding
  `audio.output.ended`, a 1.2-second PCM tail gap now closes playback and
  returns the half-duplex codec to the microphone. Intentional teardown also
  ignores the TLS close callback instead of painting it as a connection error.
- Realtime audio writes tolerate a one-second Wi-Fi/TLS scheduling stall. The
  websocket library treats its send deadline as fatal, so the previous 200 ms
  value could tear down a healthy call after a single delayed frame.
- Realtime can start after a completed or failed push-to-talk turn. Those
  terminal states remain visible on the face but no longer count as active
  voice work; an actual recording, upload, response wait or playback still
  blocks the realtime call.
- The authenticated local `/config` diagnostic reports realtime state, failure
  reason, elapsed seconds, uplink bytes/drops, speaker downlink bytes and
  completed responses.
  Physical acceptance can therefore verify the whole audio path even when the
  board's optional USB serial console is unavailable.

## 1.9.2 — 2026-08-31

### Changed
- Pair once through the exact ESP bootstrap route, store the returned opaque
  device bearer, and use `Authorization: Bearer` plus the device id thereafter.
  Principal/workspace configuration and the dedicated device-token header are
  removed; bootstrap is fixed to `anonymous/default` and later scope resolves
  from the server-side pairing record.

## 1.9.1 — 2026-08-14

### Fixed
- **Tapping the face in realtime opened the answer modal instead of calling.**
  The realtime branch sat *below* `if (mode != HANDSFREE) { answer_open();
  return; }`, so it was unreachable and a tap showed an empty "nothing yet"
  panel. Realtime is now checked first.
- **The idle timer deleted the audio peripheral underneath a running realtime
  capture.** `audio_down()` refused while a turn recording or a playback was
  open but knew nothing about the stream, so the stream task died with `read
  failed` and was restarted in a loop every three seconds. Every user of the
  hardware has to be able to hold it, or the release is a race rather than a
  release. Same defect shape as the mid-reply teardown in 1.6.3.

### Added — the microphone uplink
`audio_stream_start()` is a capture path separate from the turn loop: raw
24 kHz chunks to a callback, no staging file and no local endpointing, because
realtime lets the server decide where turns begin. The turn loop keeps its own
path untouched — sharing one capture task would have put a mode flag through
every branch of the thing this project spent longest getting right.

The two directions take turns on the codec, because one ES8311 on one I2S is
what this board physically has: the mic stops before the speaker opens and
restarts after. That is also exactly what the device told the server with
`echo_cancellation:false`, so the declaration and the hardware agree.

**Measured: 816 KB of mic audio streamed in ~18 s with 1 drop.** The uplink
works.

### Known — the call still ends without a reply
The server closes the call after ~18 s of audio and no assistant audio comes
back. The cause is now identified and is not the transport: the backend profile
sets `turn_detection_mode: none`, and `requested_realtime_turn_detection()`
returns no override when the client declares `hands_free` — so nothing ever
decides a turn is over. Audio streams forever and is never committed. Fixing it
means either driving `ptt.engage` / `ptt.release` from the device's own
endpointer, or asking for `server_vad` and giving up the server-side
half-duplex suppression. Not guessed at here.

## 1.9.0 — 2026-08-14

### Added — realtime is selectable, and a call runs
Selecting **realtime** in settings and tapping the face opens a live session:
media session, control WS, `session.start`, `session.ready`, and assistant
audio played through the ring and drain task the turn loop already uses. Tapping
again ends it; leaving the face page ends it. A call holds a TLS session and
the radio for its whole length, so it is something the owner starts and ends
deliberately, never a state the device drifts into.

**The microphone uplink is not built yet.** The call can speak to you and
cannot hear you, and the screen says exactly that — `LIVE 0:12 . no mic yet`,
and `live call . no mic yet` on the settings row.

### Step 2, measured — the audio hardware fits beside the socket
First attempt **rebooted the device mid-call**:
`i2s_alloc_dma_desc: allocate DMA buffer failed`, `ESP_ERR_NO_MEM`, `abort()`.
Two separate defects behind that:

- **The BSP was compiled to abort on any error.** With `CONFIG_BSP_ERROR_CHECK=y`
  — the default — `BSP_ERROR_CHECK_RETURN_ERR(x)` *is* `ESP_ERROR_CHECK(x)`, so
  a failed allocation restarts the device instead of returning "no audio". This
  is the same root as the touch abort patched by hand in the vendored BSP;
  turning the option off fixes the class rather than the instance, and the
  fallback paths in this firmware were already written to expect a return.
- **8 × 1024 DMA (~32 KB) does not fit beside a socket that has already spent
  ~51 KB.** The depth is now settable and realtime asks for 4 × 256; the turn
  path keeps the deep buffers that made its bursty HTTP playback smooth.

| Stage | Free | Largest block |
|---|---|---|
| idle | 98 228 | 37 888 |
| ws connected | 47 072 | 22 016 |
| **+ audio hardware (lean DMA)** | **38 072** | 22 016 |

**9 KB, not 32.** A live call with audio hardware up leaves ~38 KB.

### Unchanged
Dictate and hands free are untouched: realtime lives in `realtime.c`, nothing
on the turn path calls into it, and the deep DMA is restored when a call ends.

## 1.8.0 — 2026-08-14

### Added — realtime step 1: the socket, and what it costs
`realtime.c` opens a control WS against Magician's BackendProxied profile,
sends `session.start`, and reports free heap at every stage. **Nothing on the
working turn path calls into it, and it runs only when asked.** No audio is
captured, streamed or played yet — that is deliberate, because a realtime call
holds a TLS session for minutes and on this board every failure so far has been
heap exhaustion wearing a different mask.

**It fits.** A live session costs **~54 KB**, leaving **~48 KB free with a
26 KB largest block**, stable across a 25-second hold and **fully recovered on
close**. `session.ready` came back with the agent roster and tool catalog, so
the session is genuinely live and not merely connected.

Three costs the design note did not predict, none findable without the real
endpoint:
- **`CONFIG_WS_BUFFER_SIZE` is a compile-time IDF constant**, not the client's
  `buffer_size` field. At the 1024 default the upgrade failed with `Header size
  exceeded buffer size` while TLS had plainly succeeded — a reply through
  Cloudflare Access carries far more header than a bare origin. Now 3072.
- **The websocket task needs ≥10 KB of stack** for the mbedtls handshake; at
  6144 it failed with a bare `ws error` and no cause.
- **The deployed server does not know `surface_type: "esp_terminal"`.** Every
  other variant registers; ours returns 400. The variant was added to the
  source in stage 1 and never deployed, and nothing noticed because the turn
  path sends `source_surface` as a form string and never touches this enum. The
  spike falls back to a known variant, loudly, so the measurement could
  proceed — that is a note about the deployment, not a fix.

### Unchanged
Dictate and hands free are untouched. `MODE_REALTIME` still refuses selection.

---

Older entries: `docs/archive/changelogs/magesp.md`
