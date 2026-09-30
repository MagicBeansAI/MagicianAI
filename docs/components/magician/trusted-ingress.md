# Trusted ingress — verification the caller cannot supply

Readiness review §9 step 6: *"server-supplied verification, so engagement routing
has a real root."*

## Invariant

A verification signal supplied by the subject about itself is not a
verification signal. `channel_verified` in the chat query string can only
lower trust, never raise it; `is_owner` grants owner routing only to a sender
the server itself verified.

## What decides it now

`envoy::channel_is_verified(channel_type, request_authenticated, caller_claim)`,
from two inputs the caller does not control:

1. **`request_authenticated`** — whether the outer boundary attached a
   `VerifiedRequestIdentity`. The middleware inserts it only after Cloudflare
   Access, a paired device, or an actual loopback peer has been proved, and the
   type is deliberately not deserializable from an HTTP payload.
2. **The channel's own transport** — a phone network establishes who sent a
   WhatsApp message; SMTP does not establish a `From:` header.

| Channel | Transport names the sender? |
|---|---|
| `web` | yes — the authenticated request *is* the sender |
| `kapso`, `whatsapp`, `telegram`, `imessage`, `sms` | yes — platform-verified account or number |
| `agentmail`, `email`, `mail` | **no** — `From:` is spoofable |
| anything else | **no** — unclassified fails closed |

## The asymmetry is the mechanism

A caller **may de-escalate**: `Some(false)` is honoured, because an adapter sees
things the server cannot — AgentMail's `unauthenticated` label is exactly that
kind of knowledge.

A caller **may never escalate**. `Some(true)` on an unauthenticated request, or
on a transport that cannot name its sender, changes nothing.

Unclassified channels fail closed, because defaulting an unknown channel to
trusted would silently reopen the hole for the next channel someone adds.

## Deployment consequence — read before deploying

The access middleware is **inert unless `MAGICIAN_CF_ACCESS_TEAM_DOMAIN` and
`_AUD` are both set**. Without them it attaches an identity only for **loopback**
peers. So:

- **loopback caller** → identity present → verified;
- **non-loopback caller with no Cloudflare Access** → no identity →
  **unverified**, routed as guest to the envoy agent instead of the owner's
  thread.

An inbound adapter (e.g. a kapso webhook) that reaches the server from something
other than loopback therefore resolves as guest. The correct fix is to configure
Cloudflare Access or route the adapter over loopback — **never** to reintroduce a
caller-supplied or default-`true` verification.

## The default channel

`chat_api::resolve_inbound_routing` reads the channel from the caller and
defaults it to `web`:

```rust
let channel_type = normalized_query_value(query.channel.as_deref()).unwrap_or("web");
```

So `web` must resolve on `verified` like every other channel — an unconditional
`true` for `web` would route any request that omits `channel` as the owner. `web`
has no address to allowlist because the authenticated request *is* the sender;
a genuine local UI (Cloudflare Access, paired device, real loopback peer) gets
`channel_is_verified("web", true, None) == true`.

Channel matching is case- and whitespace-insensitive, matching
`channel_transport_authenticates_sender`. The two decide who is speaking and
must not disagree about what `Web` means.

## Related

- Outward sending policy: `docs/components/magician/outward-actions.md`.
- Owner/company split: `docs/components/magician/company-identity-split.md`.
