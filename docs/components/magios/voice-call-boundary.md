# Voice call boundary on iOS — shown, never negotiated

A realtime voice call resolves to a **boundary** on the server: which surface the
call is (`realtime_voice`, `meeting`, `public_envoy`), whether its audience is
the `owner` or `untrusted`, and which agent it is bound to. A shared room binds
the outward agent, not the owner's assistant.

`session.ready` carries that boundary so the client can show it:

```json
"boundary": {
  "surface": "meeting",
  "audience": "untrusted",
  "agent_id": "envoy",
  "elevatable": false
}
```

## Display-only, by design

`RealtimeVoiceProtocol.Boundary` mirrors the wire block and
`RealtimeVoiceProtocol.boundary(from:)` narrows it, following the same shape as
the existing `addressing(from:)` parser. `RealtimeVoiceClient` publishes it as
`boundary`, and `VoiceCallPanel` renders a non-actionable line for an untrusted
call only.

The control socket still authorizes with `MagicianAccess.authorize`, and also
offers `magician-voice-control-v1` plus `magician-bearer.<deviceToken>` so the
upgrade survives hops that drop `Authorization`. The server selects only the
application protocol and never echoes the bearer.

There is deliberately **no counterpart the client can send**. `elevatable` is
mirrored from the wire and pinned to `false` regardless of what arrives — it
exists so a reader of the type sees that the boundary is fixed, rather than
wondering whether some other flow can raise it. A control implying otherwise
would be advertising that a shared room can ask to be trusted.

## Absent means unknown, never owner

A missing or malformed `boundary` yields `nil`, and the panel then shows nothing
extra. This matters more than it looks: an older backend sends no block at all,
and the safe collapse is silence. Fabricating an owner boundary — or treating
absence as "this call is private" — is the one wrong answer a display could give,
because it would reassure the reader precisely when the runtime does not know.

An owner call also renders nothing extra, so absence of a notice is never itself
a claim.

## Related

- Server-side derivation, telemetry and what a room may retrieve:
  `docs/components/magician/realtime-media-rails.md`.
- The web counterpart: `docs/components/unified-ui/voice-call-boundary.md`.
