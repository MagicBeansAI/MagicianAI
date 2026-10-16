# magesp — ESP32-C6 voice terminal

Firmware for the physical Magician voice terminal, a Waveshare
ESP32-C6-Touch-AMOLED-1.8 on the desk: press a button, talk, hear Magician
answer — or set hands-free and skip the button.

**Current firmware:** `1.9.3`. Push-to-talk and native realtime voice work end to end.

- Source and build: [magesp/README.md](../../../magesp/README.md)
- Measured hardware: [magesp/HARDWARE.md](../../../magesp/HARDWARE.md)
- Design record, archived (stages 0–5 shipped):
  2026-08-13-esp32-voice-terminal-design.md
- Stage 0/1 plan, archived:
  2026-08-13-esp32-voice-terminal-stage0-1.md
- Active: 2026-08-14-esp32-realtime-proxied-design.md
- Next: 2026-08-15-esp32-for-you-feed.md

## Voice modes

| Mode | What it does |
|---|---|
| `dictate` | Hold the button to talk. The default. |
| `hands free` | Tap the face to start listening, tap again to stop. Endpointing is on-device. |
| `realtime` | Face tap starts/ends a BackendProxied call (`realtime.c`). |

`maze.c` reads `voice_state()` (`TURN_SENDING` / `TURN_THINKING` /
`TURN_SPEAKING`) to drive the face labels and animation.

## Pairing

The device pairs once at `POST /api/magician/v2/devices/pair`. That bootstrap
mints an opaque device bearer for the local `anonymous/default` scope. Later
calls send `Authorization: Bearer` plus `X-Magician-Device-Id`; the server
resolves principal/workspace from the pairing record. The firmware never
sends scope headers.

## Where it plugs into Magician

No extra server surface is required for a working voice loop. The device is
an HTTP client of endpoints that already ship (paths under
`/api/magician/v2` except `GET /health`):

| Endpoint | Role |
|---|---|
| `GET /health` | Reachability, every 15 s |
| `POST /api/magician/v2/media/voice-notes` | The whole turn: audio → STT → chat → reply |
| `POST /api/magician/v2/media/tts/synthesize` (`format: "pcm"`) | Reply audio |
| `POST /api/magician/v2/media/voice-notes/events` | Client-postable lifecycle events |
| `GET /api/magician/v2/media/voice/{id}/control` | Realtime control WS (`BackendProxied`) |
