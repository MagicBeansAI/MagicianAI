# Native Presence / Tray Bridge Wire Protocol

WebSocket contract between a native tray or mascot app (macOS / Windows /
Linux) and the magician backend bridge endpoint
(`GET /api/magician/v2/media/bridge/{session_id}/ws`).

The native surface is responsible for owning the user's device (screen, mic,
speakers, pointer, desktop overlay) and streaming what magician needs over this
socket.
Magician returns runtime decisions (pointer overlay commands, capability
updates, mascot/presence commands) via the same socket. Audio playback /
speaker output remains a host-side concern — provider TTS responses still come
over the standard HTTP endpoint (`/media/tts/synthesize`).

Boundary note: this protocol is for media, pointer overlays, and realtime
surface coordination. Native desktop automation should use the separate
Magician Presence, Mascot, Voice, And Screen Context Plan
so host-side `cua-driver` or equivalent providers, screen capture, pointer
guidance, ambient capture, and `osascript` / JXA remain behind an explicit
trust, permission, and audit boundary.

The macOS mascot's menu-bar controls use a separate loopback-only control
listener owned by the Swift host and called by the Tauri host gateway. Those
commands are local UI control (`ask`, `open-chat`, `summon`, `dock`, `roam`,
`replay-startup`, `quiet`, `visible`, `quit`); durable runtime state still
flows through the registered media session and this bridge.

## Connection lifecycle

1. The native app registers a realtime media session via
   `POST /api/magician/v2/media/sessions` with `surface_type` set to one of
   `tray_macos` / `tray_windows` / `tray_linux` / `mascot_macos` /
   `mascot_windows` / `mascot_linux` and `transport` set to `bridge`.
   Capabilities should reflect what the host OS actually exposes
   (`mascot_overlay`, `text_bubble`, `screen_capture`, `pointer_overlay`,
   `system_audio`, `desktop_action`, `mic`, etc.).
2. The native app connects to
   `wss://…/api/magician/v2/media/bridge/{session_id}/ws` using its registered
   session id and workspace-bound bearer.
3. Bridge actor validates that the session exists, belongs to the
   caller's scope, and has a native bridge `surface_type`. Mismatches return
   `403` / `400` before the WebSocket upgrade.
4. Once upgraded, the bridge emits `media.tray.bridge.connected` and
   begins a 30 s heartbeat (`ws::Ping`). A 120 s silence closes the
   socket.
5. Either side may close cleanly via the standard WebSocket close
   frame. The bridge emits `media.tray.bridge.disconnected`.

## Text frames (JSON)

Every text frame MUST be a JSON object with a `kind` discriminator.
Unknown `kind`s are emitted as `media.tray.bridge.error` rather than
silently ignored.

### `kind: "pointer"`

Tray → backend. Pointer-overlay commands sent by the user (e.g. "draw a
ring at (x, y)"). Routed to `media.tray.pointer.command` so downstream
listeners (active task, conversation) can react.

```json
{
  "kind": "pointer",
  "x": 1240,
  "y": 720,
  "action": "ring" | "tap" | "swipe" | "highlight",
  "duration_ms": 800,
  "metadata": { "context": "screen-id-or-window-title" }
}
```

### `kind: "transcript"` / `"transcript.delta"`

Tray → backend. Final or partial transcript fragments from a tray-local
STT pipeline (e.g. ambient audio transcribed on-device). Routed to
`media.transcript.final` and `media.transcript.delta` respectively so
the chat UI's existing transcript subscribers pick them up without a
new code path.

```json
{
  "kind": "transcript",
  "text": "open the staging dashboard please",
  "language": "en-US",
  "started_at_ms": 1747512345000,
  "ended_at_ms": 1747512347500,
  "speaker": "user" | "system" | null
}
```

```json
{
  "kind": "transcript.delta",
  "text": "open the staging",
  "is_final": false
}
```

### `kind: "capability"`

Tray → backend. The user toggled a capability on the tray (e.g.
disabled system audio capture). Routed to `media.capabilities.updated`
and the bridge actor reads it so backend code can hot-check the latest
state without polling the session record.

```json
{
  "kind": "capability",
  "capabilities": {
    "screen_capture": true,
    "system_audio": false,
    "pointer_overlay": true
  }
}
```

### `kind: "mascot"`

Native mascot -> backend. Presence/mascot status updates that are better
expressed over the bridge than as one-off HTTP client events. The backend still
records canonical lifecycle events through the media event stream.

```json
{
  "kind": "mascot",
  "state": "idle" | "sleeping" | "working" | "blocked" | "success",
  "visible": true,
  "quiet": false,
  "anchor": "docked" | "near_cursor" | "attention_corner" | "hidden"
}
```

### Backend -> native text frames

Backend pushes via `TrayDownstreamFrame::Text(json)`. The native app SHOULD
handle at minimum:

```json
{ "kind": "pointer.show", "x": 120, "y": 480, "action": "ring", "duration_ms": 1500 }
{ "kind": "pointer.hide" }
{ "kind": "mascot.state", "state": "working", "reason": "task_started" }
{ "kind": "mascot.anchor", "anchor": "near_cursor" }
{ "kind": "mascot.bubble", "action": "open", "thread_id": "thread_..." }
{ "kind": "capture.request", "channel": "screen" | "system_audio", "duration_ms": 5000 }
{ "kind": "capture.stop", "channel": "screen" | "system_audio" }
{ "kind": "capability.requested", "channel": "screen_capture" | "system_audio" }
```

The native app MUST gracefully ignore `kind`s it doesn't recognise (backend
will roll out new commands ahead of universal host support).

## Binary frames

Native app -> backend. The bridge treats binary frames opaquely and emits
`media.tray.frame.received` with the byte count for observability.
**The native app SHOULD send a `chunk.metadata` text frame immediately
before each binary frame** so backend listeners can disambiguate
screen frames from audio chunks:

```json
{
  "kind": "chunk.metadata",
  "channel": "screen" | "system_audio" | "mic",
  "encoding": "image/jpeg" | "image/webp" | "audio/pcm" | "audio/opus",
  "width": 1920,
  "height": 1200,
  "sample_rate": 16000,
  "channels": 1,
  "sequence": 18234
}
```

Backend -> native binary frames are reserved. Native apps should not assume
binary downstream traffic.

## Authentication

The WebSocket inherits the `(principal, workspace)` scope embedded in the same
bearer used by the HTTP API. The bridge validates that the magician session
referenced in the path belongs to that scope; mismatches close with
`403`.

API keys for upstream providers (OpenAI, etc.) NEVER leave the
backend — the native app talks only to magician.

## Privacy defaults

* Raw screen frames and system audio bytes are **never persisted** by
  the bridge actor. It only emits metadata events (byte count, channel,
  optional dimensions) into the observability stream.
* Native apps MUST honor the `raw_media_persistence` permission on the
  session record. When `unknown` or `denied`, captured media stays in
  the host process until a deliberate save action.

## Known limits

* No per-channel back-pressure: the bridge accepts unbounded binary frames, so a
  host faster than downstream consumers is not flow-controlled.
* Pointer commands are unsigned; a compromised host app is trusted for overlay
  commands.
