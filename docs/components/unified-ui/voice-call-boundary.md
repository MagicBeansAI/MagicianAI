# Voice call boundary — shown, never negotiated

A realtime voice call resolves to a **boundary** on the server: which surface the
call is (`realtime_voice`, `meeting`, `public_envoy`), whether its audience is
the `owner` or `untrusted`, and which agent it is bound to. A room binds the
outward agent, not the owner's assistant.

A later spoken turn cancels any still-open OpenAI `response` before
`response.create`, so the second utterance can speak without reconnecting.

Browser OpenAI DirectPeerToPeer `session.update` locks `output_modalities` to
`["audio"]` so a connected WebRTC call still speaks. Open mic never sends
`turn_detection: null`: a session minted from Hold-to-talk (`none`) falls
back to `server_vad` so "Listening" actually commits a turn. In-app
`session.start` sets `require_voice_prefix: false`, matching iOS, so ordinary
speech is admitted without a wake phrase. Backend-proxied capture
acknowledges configuration immediately so a catalog update cannot rotate the
call waiting for a WebRTC `session.updated`.

Live captions on Web, iOS, and Android use one bubble path for GPT Realtime
and Gemini Live: `transcript.user.partial` while the user speaks, and
`transcript.assistant.delta` as a growing snapshot (fragments are merged
if the provider sends them). DirectPeerToPeer GPT also paints user
transcription deltas from the OpenAI data channel. The whole turn is no
longer held until `turnComplete`.

`session.ready` carries that boundary so the client can show it:

```json
"boundary": {
  "surface": "meeting",
  "audience": "untrusted",
  "agent_id": "envoy",
  "elevatable": false
}
```

**It is display-only, and that is a design constraint rather than an omission.**
`elevatable` is a constant `false` and there is deliberately no counterpart the
client can send. A control that let a call request a different surface — or a UI
that merely implied one existed — would be advertising that a room can ask to be
trusted. Nothing in the client may branch on `boundary` to obtain different
treatment; the server decides from the registered media session and never from
anything the client says on the wire afterwards.

The value is read from the live call state on the server, not recomputed for
display, so what the UI shows and what authorization actually enforces cannot
drift apart. A diagnostic that disagreed with the boundary in force would be
worse than showing nothing.

## Client handling

`realtimeVoiceClient.ts` narrows the wire block through `readCallBoundary()` and
publishes it on `voiceCallStore` as `call.boundary`:

- an absent or malformed block yields `null` — *"we do not know"* — never a
  fabricated owner boundary, which is the one wrong answer a display could give;
- an older backend that does not send the block therefore leaves `boundary`
  `null` rather than looking like an owner call;
- it is **not** repeated on `audio.rebind`. A rotation preserves the boundary
  server-side, so the client keeps what it was told once.

`VoiceCallOverlay.svelte` renders a non-actionable notice for an untrusted call
only, telling the reader the call is a shared room and the assistant is answering
without access to their private context. An owner call renders nothing extra —
the absence of a notice is not a claim, so an unknown boundary and an owner
boundary look the same, which is the safe collapse.

## Related

- Server-side derivation, telemetry (`media.voice.surface.resolved`) and what a
  room may retrieve: `docs/components/magician/realtime-media-rails.md`.
- Resume framing on the same call: `voice-resume-framing.md`.
