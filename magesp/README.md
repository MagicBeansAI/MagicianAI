# magesp — Waveshare ESP32-C6-Touch-AMOLED-1.8 firmware

The physical Magician voice terminal: press a button, talk, hear Magician
answer. Or set it to hands free and skip the button.

**Current firmware version:** `1.9.3`.

Design record (archived — stages 0–5 shipped):
`docs/archive/plans/2026-08-13-esp32-voice-terminal-design.md`
Active: `docs/plans/2026-08-14-esp32-realtime-proxied-design.md` ·
Next: `docs/plans/2026-08-15-esp32-for-you-feed.md`

## What runs today

The voice loop sends microphone audio to Magician over HTTPS and streams the
spoken reply back: hold the button to speak, then release to submit.

| State | Screen |
|---|---|
| Idle | Tilt game, **pixel face**, the For you page, or **voice mode** — short press cycles |
| Hold BOOT | Voice orb; "opening mic" until the codec delivers, then "listening" |
| Release | sending → thinking → the reply on the face, spoken aloud |
| Tap the caption | The whole answer, wrapped and scrollable. Tap or press to exit |

A **connection dot** sits at the top of every page: green connected, orange
connecting, red failed.

The first page is a tilt game; tapping it does not move the ball. Short-press
**BOOT** to reach the face, then hold BOOT to speak and release to send. In
push-to-talk mode a face tap opens the last answer; it does not open the mic.

### Voice mode

Tap a row on the fourth page, or set it over the network:

```sh
curl -X POST http://<device-ip>/config \
  --data-urlencode "auth=<passphrase on screen>" --data-urlencode "mode=hands free"
```

**Voice lives on the face page and nowhere else.** On the game, For you and
voice-mode pages the microphone is shut, hands free will not arm, holding the
button does nothing, and the audio hardware is released. Leaving the face page
mid-utterance submits what was already said rather than discarding it.

The reason is not tidiness. A microphone that stays open behind a maze or a
settings list is one nobody is thinking about, and this device has no other way
to make its whereabouts obvious. Scoping it to the one screen that shows a face
means the mic is open only where the interface is visibly listening.

Returning to the face page resumes listening if you had switched it on — the
page cycle is navigation, not a change of mind.

| Mode | What it does |
|---|---|
| `dictate` | Hold the button to talk. The default |
| `hands free` | **Tap the face to start listening**, tap again to stop. While listening, the device decides where each turn begins and ends |
| `realtime` | Experimental streaming mode; backend qualification is required — see below |

Hands-free endpoints **locally**, on the samples: a peak meter per 64 ms frame,
a scored onset to open, 1300 ms of quiet to close, and 256 ms of pre-roll so the
recording does not start mid-syllable. Everything downstream — the staging
file, the upload, the reply, the speaker — is the push-to-talk path unchanged,
so there is one transport and one bug surface.

**The trigger level is measured, not compiled in.** Frames that do not clear
the current threshold feed a slow average of the room; the threshold is a
multiple of that average, clamped at both ends. Every fixed number tried here
was wrong in one direction or the other — 2600 answered the codec's own
power-on pop, 6000 sat *above* ordinary speech at desk distance and the device
heard nothing at all. A constant cannot know the room, the gain or the
distance.

Two further things the measurements forced:

- **Hands-free opens the mic at 27 dB, push-to-talk at 18.** 18 dB was set for
  a talker holding the device. At arm's length the same voice measured a 7035
  peak with 2 of 31 frames clearing any usable threshold — the microphone heard
  it and the endpointer could not. No threshold fixes a signal under the noise;
  the gain is the knob. Pressing the button drops it back to 18 dB, because a
  hand on the device means the old assumption is true again.
- **The onset is scored, not counted.** Requiring N *consecutive* loud frames
  missed real sentences, because a far-field speech envelope dips between
  syllables. Loud frames add 2, quiet frames subtract 1, so speech accumulates
  through its own gaps while a lone click decays away.

The alternative was streaming raw PCM to a server VAD over a second websocket.
That buys a better endpointer at the cost of a **permanent second TLS session**
on a part with ~128 KB of free heap, plus a continuously-transmitting radio, to
learn something a peak meter already knows on-device. Not worth it here.

**Realtime has firmware session, playback and microphone-uplink code, but is
not qualified on the selected Linux backend.** The *direct* profile negotiates
`DirectPeerToPeer`, which is WebRTC (ICE, DTLS-SRTP, Opus) and this chip cannot
host it. But Magician also ships `voice_realtime_openai_backend`, a
**BackendProxied** profile whose control WS at
`/media/voice/{id}/control` carries **raw PCM16 in binary frames** — nothing
here the C6 cannot speak.

The blocker I expected to be fatal is already handled server-side. Realtime
means the speaker plays while the mic is open, which normally demands acoustic
echo cancellation — and ESP-SR's AEC does not support the C6 (no Xtensa DSP).
But `should_suppress_half_duplex_input()` drops client PCM upstream while the
assistant is speaking, described in the source as *"an authoritative transport
boundary… echoed PCM must never reach either local STT or the cascaded provider
while it is speaking"*. A half-duplex client needs no AEC, and this firmware is
already half-duplex: `audio_arm()` refuses while the speaker is open.

The existing path uses:

| | |
|---|---|
| WSS client | `esp_websocket_client`, one persistent TLS session — the real heap risk, ~111 KB free today and the probe already yields for one |
| Duplex PCM16 24 kHz | The ES8311 is a duplex codec on one I2S and both directions run at one rate, so this fits |
| Playback | The existing 24 KB ring and drain task take it unchanged |
| Server | A backend profile and available speech providers for the selected audio surface |

The mode is selectable; selection does not establish backend readiness. On
September 17 the physical C6 reached the Linux control WebSocket but closed
before `session.ready`: its `hands_free` request selected the cascaded provider,
whose required `fluid-silero-v6` VAD was unavailable on Linux. Push-to-talk was
separately verified with real microphone input and audible WAV playback. See the
mobile acceptance plan
for the remaining realtime checks.

A held button still outranks the gate in any mode: pressing forces the
recording open immediately and holds it until release.

**Choosing hands free does not open the microphone.** It arms on a tap on the
face and stops on another tap, and the choice is not persisted across a reboot
— a mic that reopens itself after a power cycle is not something anyone agreed
to. A **line of its own** on the face says which state it is in —
`TAP THE FACE TO LISTEN` / `LISTENING` / `HEARING YOU` — deliberately not the
caption, because the caption holds the last answer and an answer outlives the
turn that produced it. A **second dot** at the top reports the microphone
itself: hollow amber while it is merely open, solid red
once it is keeping what it hears. That dot is driven from the capture state and
never from the setting — the two are different facts, and confusing them is the
defect this project has now found ten times.

Trade-off worth saying out loud: while listening, hands-free keeps the
microphone open, so it costs power and it will occasionally hear the room. A
false trigger reaches the transcriber, comes back `empty_transcript`, and shows
as "didn't catch that".

### Theme

`theme=dark` in the setup form or via `/config`, persisted and applied live:

```sh
curl -X POST http://<device-ip>/config \
  --data-urlencode "auth=<passphrase on screen>" --data-urlencode "theme=dark"
```

Dark uses **true black**. On AMOLED a black pixel is unlit, so this is a real
power setting — a light screen costs roughly 3–5× the panel draw of a dark one.
Irrelevant on USB; dominant if a LiPo is fitted to the MX1.25 header.

Colours are not literals: objects register the **role** they play and a theme
swap walks that registry. A hardcoded `lv_color_hex(0x…)` will silently ignore
the theme — that bug cost an afternoon, and the boot-time screen-tree dump
(every object's *computed* colour) is what found it.

**Tilt levelling.** Whatever orientation the board is in when the game page
opens becomes "level". It re-levels itself if it detects a stale reference —
levelled on edge then laid flat reads nearly a full `g` of false slope — and
after each solve. Press twice to leave and re-enter the page to force one.

Temperature (QMI8658 die sensor) shows top-centre in every state.

**Screen sleep:** dims to 10% after 25 s, dark after 75 s. Wakes on a button
press or on the board being moved. A press that wakes a dark screen only wakes
it — it will not also switch page or open the mic. Nothing sleeps mid-turn.

> **The panel has rounded corners.** Anything drawn into a screen corner is
> hidden by the glass, so screens are laid out to a safe area and the game's
> playfield is a circular arena (r = 176 px) rather than the screen rectangle.
> See `HARDWARE.md`.

### Identity

The device pairs once with `POST /devices/pair`. That narrowly scoped bootstrap
cannot choose a principal or workspace: it mints a device bearer for the local
single-user `anonymous/default` scope. The bootstrap is accepted only from an
actual direct loopback peer (not a loopback reverse-proxy connection carrying
forwarded-client metadata) or behind a verified Cloudflare Access assertion;
merely reaching the origin cannot mint a device credential. Every later API and realtime request
sends `Authorization: Bearer <device-token>` plus `X-Magician-Device-Id`; the
device id locates the pairing record, while the bearer authenticates it and the
server resolves principal/workspace from that record. The firmware never sends
scope headers or scope query parameters. Cloudflare Access remains an optional
outer gateway and is not a substitute for the per-device bearer.

Changing the backend through `/config` or the setup form saves the URL and its
credentials together, then restarts the device. A different origin discards the
old device bearer and Access credentials; supply the new Access pair together
when required. Both configuration routes require the device passphrase shown on
screen and refuse a connection change during an active voice turn. Existing NVS
connection fields migrate on boot. A rejected/revoked bearer stays rejected:
an owner can explicitly request another bootstrap with `repair=1` on `/config`.
“Connected” requires `/devices/me` to verify this device and its assigned scope.

### Endpoints it calls

Everything sits under `/api/magician/v2` — the paths in `magician-bin/src/main.rs` are
registered inside a `web::scope`, so the string there is only the tail.
`GET /health` is the exception and genuinely lives at the root.

| Endpoint | Role |
|---|---|
| `GET /api/magician/v2/devices/me` | Authenticated device/scope verification, every 15 s |
| `POST /api/magician/v2/media/voice-notes` | The whole turn: audio → transcript → reply |
| `POST /api/magician/v2/media/tts/synthesize` | The reply as speech |

> > **A reply is one TTS request per sentence, so the gap between sentences is a
> network round trip.** Any idle timer short enough to save power is shorter
> than that gap, so the audio hardware is held explicitly for the whole reply
> (`audio_hold()`) rather than released on a timer between sentences. Playback
> also counts underruns and reports `playback ran dry N time(s)` on close —
> stuttering and a finished reply are otherwise indistinguishable from outside
> the device.

> **Replies are spoken from `assistant_speech_segments`, not
> `assistant_preview`.** The preview is truncated to 600 characters
> server-side; the segments carry the whole answer, one sentence each, which is
> what makes reply length unbounded on a device with no room to buffer it.

> **Request WAV for the existing streaming decoder.** `format: "wav"` makes
> Linux providers return a header with the sample rate and encoding. Raw PCM
> has no header and is rejected by this decoder. It handles both PCM16 WAV
> (including the tested 24 kHz Linux response) and macOS AVSpeechSynthesizer's
> float32 WAV, which can place the payload behind `JUNK` and `FLLR` padding.
> The header is parsed and samples converted before they reach I2S.

### Memory is the binding constraint

512 KB, no PSRAM, with LVGL, wifi, TLS and audio all resident. Two facts worth
carrying:

- **Two TLS sessions do not fit.** The reachability probe yields while a turn
  runs rather than competing for the heap a handshake needs.
- **The LVGL draw buffer is the biggest single lever.**
  `CONFIG_BSP_DISPLAY_LVGL_BUF_HEIGHT` at its default 100 cost 74 KB; it is now
  28 rows.
- **Do not reach for `CONFIG_MBEDTLS_DYNAMIC_BUFFER`.** It survives a handshake
  in less heap, but reallocates the record buffer per record and fails part-way
  through a large streamed download. Free heap for static buffers instead.
- Pages are free. The draw buffer is scratch space sized by the panel, not a
  framebuffer per page — the entire pixel-face page cost 16 bytes.

Free heap is logged at the start and end of every turn. Watch it before adding
anything resident.

## First-time WiFi setup

Credentials are **never** in the build. On a device with none stored, it raises
its own access point:

1. Join **`magesp-XXXX`**. It is WPA2 and the **passphrase is shown on the
   device's own screen**. The current passphrase is derived from the device MAC
   and survives reboots; it is not a random secret or proof of physical ownership.
2. Open <http://192.168.4.1> in any browser.
3. Pick your network from the scanned list, enter the password, and give the
   **Magician base URL** (e.g. `http://192.168.1.20:PORT`).
4. Enter the device passphrase shown on its screen and save. The device restarts and joins.

Everything lands in NVS under the `magesp` namespace. If the stored network
later refuses it — a moved router, a changed password — the device **falls back
to its setup AP** rather than sitting dark, so recovering never needs a reflash.

The **For you** page doubles as the network status readout:

| Screen | Meaning |
|---|---|
| `set up wifi` + AP name | No credentials stored |
| `connecting` | Joining, with attempt count |
| `wifi failed` | Network refused us; AP is coming back |
| `magician unreachable` + URL | On the network, backend did not answer |
| `connected` | Magician authenticated this paired device and returned its scope |

The authenticated connection is rechecked every 15 s against `<base>/api/magician/v2/devices/me`.

### Reaching Magician from outside your LAN

The base URL may be **https**; the public-CA bundle is compiled in, so a
Cloudflare-tunnel hostname verifies with no pinning. That is what makes the
device work on a phone hotspot, in an office, or anywhere off the home network.

Two things to know:

- **The ESP32-C6 radio is 2.4 GHz only.** Recent iPhones default their Personal
  Hotspot to 5 GHz — turn on **Maximize Compatibility** or the device will not
  see the hotspot at all. Android usually has an equivalent band setting.
- A LAN IP (`http://192.168.x.x:3002`) only works while the device and the
  server share a network. On a hotspot they do not, so the tunnel URL is
  required, not merely nicer.

Exposing Magician publicly puts an API that can create tasks and read notes on
the internet. Provision the device with a workspace-bound bearer; it sends the
credential in `Authorization` and does not assert principal/workspace headers.

## Help page on the device

The device serves its own help at **`http://<device-ip>/help`** — setup, which
base URL to use where, the endpoints it calls, buttons, and a failure table.
Plain responsive HTML, so any Android or iOS browser opens it with nothing
installed. During setup it is at `http://192.168.4.1/help`.

The address is shown on the **For you** page, and `/` re-runs provisioning at
any time.

## Build and flash

Requires ESP-IDF **v5.5.1**.

From the repository root, `make test-magesp-connection` checks the production
connection parser, backend-switch rules and identity-response validation with
sanitizers. `make build-magesp` builds with one job under `$CARGO_TARGET_DIR/magesp`
(override `MAGESP_BUILD_DIR`), keeping compiler temporary files there too. Both
use the installed SDK at `IDF_PATH`, defaulting to `~/esp/esp-idf`. These checks
do not replace physical pairing, reboot and audio acceptance.

```sh
. ~/esp/esp-idf/export.sh
idf.py set-target esp32c6
idf.py build
idf.py -p /dev/cu.usbmodemXXXX -b 460800 flash
```

> **macOS toolchain gotcha.** `install.sh` fails with
> `SSL: CERTIFICATE_VERIFY_FAILED` because python.org's Python has no usable CA
> store. Fix before installing:
> `export SSL_CERT_FILE=$(python3 -c "import certifi;print(certifi.where())")`

> **Reading serial.** The C6's USB-Serial/JTAG **re-enumerates on reset**, so a
> naive reader dies with `Errno 6 Device not configured`. Any capture script
> must reconnect across the drop.

> **BOOT is a strapping pin.** Holding the PTT button while powering on enters
> download mode instead of booting. Never instruct a user to hold it during a
> power cycle.

## Audio hardware comes up per turn, not at boot

`bsp_audio_init()` at boot and never undone cost **~30 C of die temperature**:
77 C idle, against 44 C with the subsystem down. The I2S peripheral stayed
instantiated whether or not anything was playing, and this device is idle
almost all of its life.

Three plausible explanations were wrong before the measurement found it —
worth recording so nobody re-derives them:

| Tried | Idle trend from a hot start |
|---|---|
| Power amplifier forced off at boot | still rising |
| I2S channels `disable`d at boot (returned `ESP_OK`) | still rising |
| ES8311 codec handles never created | still rising |
| `audio_init()` never called at all | **falling** |

**Disabling the I2S channels does not recover the power.** The cost is in the
peripheral existing, not in the channel being enabled — which is why
`bsp_audio_deinit()` had to be written (upstream is bring-up only) rather than
just gating the clocks.

The staging file, the playback ring and the drain task still live from boot, so
a press never has to allocate before it can record.

**The cost is 18 ms up, 0 ms down**, timed on hardware and printed at boot. A
turn pays it twice — mic, then speaker — so ~36 ms in total, against the 800 ms
the button already waits before recording. No grace period is warranted; if
that changes, keep the hardware up for a few seconds after a turn rather than
returning to bringing it up at boot.

Boot does one up/down cycle as a **self-test**: lazy bring-up would otherwise
have moved "the codec is missing" from a loud boot failure to a failure on the
owner's first press.

> **The power amplifier is a TCA9554 output latch** — its own chip, its own
> supply — so it survives an MCU reset. `bsp_audio_init()` switches it on and
> only a completed playback switched it off, so a crash mid-reply left it
> driving the speaker until someone pulled the cable. It is now explicitly off
> whenever playback is not open.

## The touch driver is patched too — and the patch is not in `managed_components/`

`components/esp_lcd_touch_ft5x06/` is a local copy of
`espressif/esp_lcd_touch_ft5x06`, wired in through `override_path` in
`main/idf_component.yml`. **`managed_components/` is gitignored and
regenerated**, so a patch applied there survives nothing.

`touch_ft5x06_init()` writes nine **tuning** registers — detection thresholds,
scan periods, water rejection. None is required to read a coordinate, and the
read path never looks at them. This board is a V1 carrying an **FT3168**: it
speaks the FT5x06 *data* registers, which is why the vendor BSP drives it with
this driver, but it NAKs some FT5x06 tuning addresses. Upstream ORs those
failures into one status and returns it, and `bsp_touch_new()` then deletes the
whole touch device — so the device booted display-only and **every tap on every
screen went nowhere for the life of the firmware**:

```
E FT5x06: esp_lcd_touch_new_i2c_ft5x06(161): FT5x06 init failed
W Touch panel did not answer -- running display-only
```

Patched to log the rejection and keep the device. If the panel truly cannot
talk, the read path now fails visibly instead of the tuning path failing
silently and taking the working part with it.

> **The controller does not answer every boot.** One boot logs
> `Detected board variant: V1 (SH8601 + FT5x06)`; the next logs `No touch
> controller at 0x15 or 0x38`. Detection is not a reliable signal about this
> part, which is why the driver is no longer allowed to throw the device away
> on the strength of it.

## The vendored BSP is patched — do not blindly re-fetch it

`components/esp32_c6_touch_amoled_1_8/` is `waveshare/esp32_c6_touch_amoled_1_8`
v1.0.0 from the ESP Component Registry, **with two local patches**. Pulling the
upstream component again silently loses both.

Upstream couples the display to the touch panel twice, so a board whose touch is
flaky cannot light its screen at all:

1. **`bsp_board_detect()`** returned `UNKNOWN` when no touch controller
   answered, and `bsp_display_new()` refuses to initialise an unknown variant.
   Patched to fall back to SH8601 (V1) with a warning so the display still
   starts.
2. **`bsp_display_indev_init()`** ran `BSP_ERROR_CHECK_RETURN_NULL(bsp_touch_new(...))`,
   which **aborts the whole app** when touch fails — the exact cause of the
   boot loop this board shipped with. Patched to return `NULL` and log, so the
   device degrades to display-only.

Both patches are marked with comments in the source.

## Tools

`tools/i2cscan/` — standalone I2C bus scanner for this board. Useful, with one
caveat learned the hard way: **a bare scan cannot see the touch controller**,
because the BSP resets `LCD_RST` and `TOUCH_RST` together through an initialised
TCA9554 before probing. Do not conclude a part is missing from this tool alone.
See §12 of the design doc.

## Tuning

| Knob | In `main/maze.c` | Note |
|---|---|---|
| Tilt sensitivity | `ACCEL_GAIN` | 180. Lower is gentler |
| Axis direction | `AXIS_X_SRC` / `AXIS_Y_SRC` | Horizontal is negated for this board |
| Maze size | `RINGS`, `RING_T`, `GAP_ARC_PX` | 4 concentric rings, 4 px thick, 40 px gaps |
| Temperature trim | `TEMP_OFFSET_C` | Die sensor reads warm; calibrate against a thermometer |

| Knob | In `main/audio.c` | Note |
|---|---|---|
| Trigger sensitivity | `VAD_MULT` | Threshold = this × the measured room floor. 6 |
| Sensitivity clamps | `VAD_FLOOR_MIN` / `VAD_FLOOR_MAX` | A silent room must not become hair-trigger; a loud one must not go deaf |
| Onset score | `ONSET_SCORE` / `ONSET_HIT` | Loud frames add 2, quiet subtract 1, opens at 5. **This** rejects impulses, not the threshold |
| Turn end | `HANGOVER_MS` | 1300 ms of quiet. 900 sat inside an ordinary pause for breath and cut talkers off. Err long: latency is cheaper than a truncated instruction |
| Codec settle | `SETTLE_FRAMES` | Applied **only** when the codec was just powered. Hands free re-arms after every reply and the codec is usually still up, so settling anyway discarded ~380 ms of real speech |
| Mic gain | `MIC_GAIN_DB` / `HF_MIC_GAIN_DB` | 18 dB in the hand, 27 dB across the desk |
| Length caps | `MAX_SECONDS` / `HF_MAX_SECONDS` | 12 s and 15 s. A 20 s capture once failed its TLS handshake, but that was heap exhaustion at handshake time, not payload size — the body streams from the file against a real `Content-Length` |

The device logs the floor and the score every two seconds while listening, so
tuning is a serial capture rather than a guess:
`gate shut; peak 1781, 2/31 frames over 3042 (floor 507), best score 2 of 5`

## Recovery

A full 16 MB factory flash image was taken before this board was first written
to. It lives **outside the repo** (too large to track):

`~/esp/board-backups/ESP32-C6-AMOLED-1.8-factory-full-16MB-2026-08-13.bin`
`sha256 1c41362d3585…`

Restore with `esptool --port … write-flash 0 <that file>`. Note the factory
image boot-loops on this unit whenever the touch panel does not answer.
